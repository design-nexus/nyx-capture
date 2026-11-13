//! SQLite library and FTS5 index for capture history.

use crate::search::{View, fts_match};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Longest side of a history thumbnail, in pixels.
pub const THUMB_EDGE: u32 = 320;

const SCHEMA: &str = r#"
CREATE TABLE captures (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  width INTEGER,
  height INTEGER,
  path TEXT NOT NULL,
  thumb_path TEXT,
  dhash INTEGER,
  starred INTEGER NOT NULL DEFAULT 0,
  pinned INTEGER NOT NULL DEFAULT 0,
  sensitive INTEGER NOT NULL DEFAULT 0,
  source TEXT,
  ocr_engine TEXT,
  ocr_status TEXT,
  ocr TEXT NOT NULL DEFAULT '',
  title TEXT NOT NULL DEFAULT ''
);
CREATE VIRTUAL TABLE captures_fts USING fts5(
  ocr, title,
  content='captures', content_rowid='rowid', tokenize='unicode61'
);
CREATE TRIGGER captures_ai AFTER INSERT ON captures BEGIN
  INSERT INTO captures_fts(rowid, ocr, title) VALUES (new.rowid, new.ocr, new.title);
END;
CREATE TRIGGER captures_ad AFTER DELETE ON captures BEGIN
  INSERT INTO captures_fts(captures_fts, rowid, ocr, title) VALUES ('delete', old.rowid, old.ocr, old.title);
END;
CREATE TRIGGER captures_au AFTER UPDATE ON captures BEGIN
  INSERT INTO captures_fts(captures_fts, rowid, ocr, title) VALUES ('delete', old.rowid, old.ocr, old.title);
  INSERT INTO captures_fts(rowid, ocr, title) VALUES (new.rowid, new.ocr, new.title);
END;
CREATE INDEX captures_created ON captures(created_at DESC);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    Screenshot,
    Recording,
}

impl CaptureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Screenshot => "screenshot",
            Self::Recording => "recording",
        }
    }
    fn parse(s: &str) -> Self {
        if s == "recording" { Self::Recording } else { Self::Screenshot }
    }
}

#[derive(Debug, Clone)]
pub struct CaptureRow {
    pub id: String,
    pub kind: CaptureKind,
    pub created_at: i64,
    pub width: i32,
    pub height: i32,
    pub path: PathBuf,
    pub starred: bool,
    pub ocr_status: String,
    pub title: String,
    pub sensitive: bool,
}

pub struct NewCapture {
    pub kind: CaptureKind,
    pub width: i32,
    pub height: i32,
    pub path: PathBuf,
    pub source: String,
}

pub struct Db {
    conn: Connection,
}

pub fn open(path: &Path) -> Result<Db> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    // Background writers (insert, OCR, star) share the file; wait for each other.
    conn.busy_timeout(std::time::Duration::from_secs(3))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch("PRAGMA user_version=2;")?;
    }
    ensure_column(&conn, "captures", "sensitive", "INTEGER NOT NULL DEFAULT 0")?;
    if version == 1 {
        // The Pinned shelf is gone; what was pinned is starred, so retention keeps it.
        conn.execute_batch("UPDATE captures SET starred=1 WHERE pinned=1; PRAGMA user_version=2;")?;
    }
    Ok(Db { conn })
}

/// Read the library without changing it. A WAL database read-only needs its `-shm`
/// file; when Capture isn't running and the folder can't be written (Glimpse's sandbox),
/// it's opened as immutable instead.
pub fn open_readonly(path: &Path) -> Result<Db> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(path, flags)
        .and_then(|c| c.query_row("SELECT count(*) FROM captures", [], |_| Ok(())).map(|_| c))
        .or_else(|_| {
            let uri = format!("file:{}?immutable=1", path.to_string_lossy().replace('?', "%3f").replace('#', "%23"));
            Connection::open_with_flags(uri, flags)
        })?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(Db { conn })
}

impl Db {
    pub fn insert(&self, item: &NewCapture) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO captures(id, kind, created_at, width, height, path, source, ocr_status, title)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', '')",
            params![id, item.kind.as_str(), now, item.width, item.height, item.path.to_string_lossy(), item.source],
        )?;
        Ok(id)
    }

    /// Write the original PNG under `dir/{id}.png` and index it. The file is never overwritten later.
    pub fn insert_png(&self, dir: &Path, png: &[u8], width: i32, height: i32, source: &str) -> Result<(String, PathBuf)> {
        std::fs::create_dir_all(dir)?;
        let id = Uuid::new_v4().to_string();
        let path = dir.join(format!("{id}.png"));
        crate::atomic_write(&path, png)?;
        let now = now_secs();
        let hash = dhash_png(png);
        self.conn.execute(
            "INSERT INTO captures(id, kind, created_at, width, height, path, source, ocr_status, title, dhash)
             VALUES (?1, 'screenshot', ?2, ?3, ?4, ?5, ?6, 'pending', '', ?7)",
            params![id, now, width, height, path.to_string_lossy(), source, hash],
        )?;
        Ok((id, path))
    }

    pub fn set_title(&self, id: &str, title: &str) -> Result<()> {
        self.conn.execute("UPDATE captures SET title=?2 WHERE id=?1", params![id, title])?;
        Ok(())
    }

    pub fn dhash(&self, id: &str) -> Result<Option<i64>> {
        self.conn
            .query_row("SELECT dhash FROM captures WHERE id=?1", params![id], |r| r.get(0))
            .optional()
            .map_err(Into::into)
    }

    pub fn set_ocr(&self, id: &str, engine: &str, text: &str) -> Result<()> {
        if self.get(id)?.is_some_and(|r| r.sensitive) {
            return Ok(());
        }
        self.conn.execute(
            "UPDATE captures SET ocr_engine=?2, ocr_status='done', ocr=?3 WHERE id=?1",
            params![id, engine, text],
        )?;
        if let Ok(Some(row)) = self.get(id)
            && row.path.is_file()
        {
            let _ = crate::atomic_write(&ocr_sidecar(&row.path), text.as_bytes());
        }
        Ok(())
    }

    pub fn ocr_text(&self, id: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT ocr FROM captures WHERE id=?1", params![id], |r| r.get(0))
            .optional()
            .map_err(Into::into)
    }

    pub fn get(&self, id: &str) -> Result<Option<CaptureRow>> {
        self.conn
            .query_row(
                "SELECT id, kind, created_at, width, height, path, starred, ocr_status, title, sensitive FROM captures WHERE id=?1",
                params![id],
                row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list(&self, view: View, query: &str, limit: i64) -> Result<Vec<CaptureRow>> {
        let mut sql = String::from(
            "SELECT c.id, c.kind, c.created_at, c.width, c.height, c.path, c.starred, c.ocr_status, c.title, c.sensitive
             FROM captures c",
        );
        let mut where_sql = Vec::new();
        match view {
            View::Starred => where_sql.push("c.starred=1"),
            View::Screenshots => where_sql.push("c.kind='screenshot'"),
            View::Recordings => where_sql.push("c.kind='recording'"),
            View::Recent => {}
        }
        if let Some(m) = fts_match(query) {
            sql.push_str(" JOIN captures_fts f ON f.rowid=c.rowid");
            where_sql.push("captures_fts MATCH ?1");
            sql.push_str(" WHERE ");
            sql.push_str(&where_sql.join(" AND "));
            sql.push_str(" ORDER BY c.created_at DESC LIMIT ?2");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![m, limit], row_from)?;
            rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
        } else {
            if !where_sql.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&where_sql.join(" AND "));
            }
            sql.push_str(" ORDER BY c.created_at DESC LIMIT ?1");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![limit], row_from)?;
            rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
        }
    }

    /// How many captures history holds.
    pub fn count(&self) -> Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM captures", [], |r| r.get(0)).map_err(Into::into)
    }

    pub fn set_starred(&self, id: &str, on: bool) -> Result<()> {
        self.conn.execute("UPDATE captures SET starred=?2 WHERE id=?1", params![id, on as i32])?;
        Ok(())
    }

    pub fn set_sensitive(&self, id: &str, on: bool) -> Result<()> {
        if on {
            self.conn.execute("UPDATE captures SET sensitive=1, ocr='' WHERE id=?1", params![id])?;
            if let Ok(Some(row)) = self.get(id) {
                let _ = std::fs::remove_file(ocr_sidecar(&row.path));
            }
        } else {
            self.conn.execute("UPDATE captures SET sensitive=0 WHERE id=?1", params![id])?;
        }
        Ok(())
    }

    pub fn get_by_path(&self, path: &Path) -> Result<Option<CaptureRow>> {
        let as_str = path.to_string_lossy();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.conn
            .query_row(
                "SELECT id, kind, created_at, width, height, path, starred, ocr_status, title, sensitive
                 FROM captures WHERE path=?1 OR path LIKE '%/' || ?2 ESCAPE '\\' ORDER BY created_at DESC LIMIT 1",
                params![as_str.as_ref(), like_literal(name)],
                row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Write `id`'s thumbnail to `dest` (from the picture with its marks burned in) and
    /// record it, so deleting or pruning the capture removes it. `fresh` rewrites one
    /// already there (after an edit).
    pub fn thumb(&self, id: &str, src: &Path, dest: &Path, fresh: bool) -> Result<()> {
        if fresh {
            let _ = std::fs::remove_file(dest);
        }
        // A thumbnail from before the last edit (or from before thumbnails showed edits) is
        // remade, so it never shows what an edit covered.
        let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let stale = modified(&crate::annot::sidecar_path(src)).zip(modified(dest)).is_some_and(|(edit, thumb)| edit > thumb);
        if dest.exists() && !stale {
            return self.set_thumb(id, dest);
        }
        if crate::export::is_edited(src) {
            write_thumb_image(&crate::export::flatten_png(src)?, dest, THUMB_EDGE)?;
        } else {
            write_thumb(src, dest, THUMB_EDGE)?;
        }
        self.set_thumb(id, dest)
    }

    pub fn set_thumb(&self, id: &str, path: &Path) -> Result<()> {
        self.conn.execute("UPDATE captures SET thumb_path=?2 WHERE id=?1", params![id, path.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// Drop the row, then the PNG, sidecar, and thumbnail files.
    pub fn delete(&self, id: &str) -> Result<Vec<PathBuf>> {
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row("SELECT path, thumb_path FROM captures WHERE id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        self.conn.execute("DELETE FROM captures WHERE id=?1", params![id])?;
        let Some((path, thumb)) = row else { return Ok(Vec::new()) };
        Ok(unlink_capture(Path::new(&path), thumb.as_deref().map(Path::new)))
    }

    /// Remove unstarred screenshots older than `days`. 0 keeps everything. Starred rows
    /// stay, and so do recordings: they're in the Videos folder, the person's own files.
    pub fn prune(&self, days: i64) -> Result<Vec<PathBuf>> {
        if days <= 0 {
            return Ok(Vec::new());
        }
        self.remove_before(now_secs() - days * 86400)
    }

    /// Remove every unstarred screenshot and its text, whatever its age.
    pub fn clear(&self) -> Result<Vec<PathBuf>> {
        self.remove_before(i64::MAX)
    }

    fn remove_before(&self, cutoff: i64) -> Result<Vec<PathBuf>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, thumb_path FROM captures WHERE starred=0 AND kind='screenshot' AND created_at < ?1")?;
        let doomed: Vec<(String, String, Option<String>)> = stmt
            .query_map(params![cutoff], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut paths = Vec::new();
        for (id, path, thumb) in doomed {
            self.conn.execute("DELETE FROM captures WHERE id=?1", params![id])?;
            paths.extend(unlink_capture(Path::new(&path), thumb.as_deref().map(Path::new)));
        }
        Ok(paths)
    }
}

/// A `LIKE` pattern matching `s` literally (`%`, `_` and `\` escaped with `\`).
fn like_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// JPEG thumbnail of a picture already in memory.
pub fn write_thumb_image(img: &image::DynamicImage, dest: &Path, edge: u32) -> Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let thumb = image::DynamicImage::ImageRgb8(img.thumbnail(edge, edge).to_rgb8());
    let mut buf = Cursor::new(Vec::new());
    thumb.write_to(&mut buf, image::ImageFormat::Jpeg)?;
    crate::atomic_write(dest, &buf.into_inner())
}

/// JPEG thumbnail of `src` written to `dest`. Skips the write when `dest` already exists.
pub fn write_thumb(src: &Path, dest: &Path, edge: u32) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Ok(img) = image::open(src) {
        let thumb = img.thumbnail(edge, edge);
        let mut buf = Cursor::new(Vec::new());
        thumb.write_to(&mut buf, image::ImageFormat::Jpeg)?;
        return crate::atomic_write(dest, &buf.into_inner());
    }
    if !crate::detect::present("ffmpeg") {
        anyhow::bail!("no thumbnail for {}", src.display());
    }
    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-ss", "0.2", "-i"])
        .arg(src)
        .args(["-frames:v", "1", "-vf", &format!("scale={edge}:-2"), "-q:v", "3"])
        .arg(dest)
        .status()?;
    if status.success() && dest.exists() {
        Ok(())
    } else {
        anyhow::bail!("ffmpeg thumbnail failed")
    }
}

pub fn ocr_sidecar(path: &Path) -> PathBuf {
    path.with_extension("ocr.txt")
}

fn ensure_column(conn: &Connection, table: &str, name: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt.query_map([], |r| r.get::<_, String>(1))?.any(|n| n.ok().as_deref() == Some(name));
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {decl}"), [])?;
    }
    Ok(())
}

fn unlink_capture(path: &Path, thumb: Option<&Path>) -> Vec<PathBuf> {
    let mut files = vec![path.to_path_buf(), crate::annot::sidecar_path(path), ocr_sidecar(path)];
    if let Some(t) = thumb {
        files.push(t.to_path_buf());
    }
    for f in &files {
        let _ = std::fs::remove_file(f);
    }
    files
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<CaptureRow> {
    let kind: String = r.get(1)?;
    let path: String = r.get(5)?;
    Ok(CaptureRow {
        id: r.get(0)?,
        kind: CaptureKind::parse(&kind),
        created_at: r.get(2)?,
        width: r.get(3)?,
        height: r.get(4)?,
        path: PathBuf::from(path),
        starred: r.get::<_, i32>(6)? != 0,
        ocr_status: r.get(7)?,
        title: r.get(8)?,
        sensitive: r.get::<_, i32>(9)? != 0,
    })
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 64-bit difference hash. Same picture → same value; a solid field hashes to 0.
pub fn dhash(img: &image::DynamicImage) -> i64 {
    let small = img.resize_exact(9, 8, image::imageops::FilterType::Triangle).to_luma8();
    let mut bits: u64 = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0] {
                bits |= 1 << (y * 8 + x);
            }
        }
    }
    bits as i64
}

fn dhash_png(png: &[u8]) -> Option<i64> {
    image::load_from_memory(png).ok().map(|img| dhash(&img))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::View;

    #[test]
    fn insert_search_prune() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir.join("capture.sqlite")).unwrap();
        let id = db
            .insert(&NewCapture {
                kind: CaptureKind::Screenshot,
                width: 10,
                height: 10,
                path: dir.join("a.png"),
                source: "grim".into(),
            })
            .unwrap();
        db.set_ocr(&id, "tesseract", "kernel panic on boot").unwrap();
        assert_eq!(db.count().unwrap(), 1);
        let found = db.list(View::Recent, "kernel", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, id);
        db.conn.execute("UPDATE captures SET created_at=1 WHERE id=?1", params![id]).unwrap();
        let pruned = db.prune(1).unwrap();
        assert_eq!(pruned.len(), 3);
        assert!(db.list(View::Recent, "", 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_the_png_and_sidecar() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-del-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("shot.png");
        std::fs::write(&png, b"png").unwrap();
        std::fs::write(crate::annot::sidecar_path(&png), b"{}").unwrap();
        let db = open(&dir.join("capture.sqlite")).unwrap();
        let id = db
            .insert(&NewCapture {
                kind: CaptureKind::Screenshot,
                width: 8,
                height: 8,
                path: png.clone(),
                source: "grim".into(),
            })
            .unwrap();
        let gone = db.delete(&id).unwrap();
        assert!(gone.contains(&png));
        assert!(!png.exists());
        assert!(!crate::annot::sidecar_path(&png).exists());
        assert!(db.get(&id).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sensitive_drops_ocr_and_sidecar() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-sens-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("shot.png");
        std::fs::write(&png, b"png").unwrap();
        let db = open(&dir.join("capture.sqlite")).unwrap();
        let id = db
            .insert(&NewCapture {
                kind: CaptureKind::Screenshot,
                width: 8,
                height: 8,
                path: png.clone(),
                source: "grim".into(),
            })
            .unwrap();
        db.set_ocr(&id, "tesseract", "ken@example.com").unwrap();
        assert_eq!(db.ocr_text(&id).unwrap().as_deref(), Some("ken@example.com"));
        assert!(ocr_sidecar(&png).is_file());
        assert_eq!(db.get_by_path(&png).unwrap().unwrap().id, id);
        db.set_sensitive(&id, true).unwrap();
        assert!(db.get(&id).unwrap().unwrap().sensitive);
        assert_eq!(db.ocr_text(&id).unwrap().as_deref(), Some(""));
        assert!(!ocr_sidecar(&png).exists());
        db.set_ocr(&id, "tesseract", "ken@example.com").unwrap();
        assert_eq!(db.ocr_text(&id).unwrap().as_deref(), Some(""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_lookup_takes_wildcards_literally() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-like-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = open(&dir.join("capture.sqlite")).unwrap();
        let add = |name: &str| {
            db.insert(&NewCapture { kind: CaptureKind::Screenshot, width: 1, height: 1, path: dir.join(name), source: "grim".into() })
                .unwrap()
        };
        let plain = add("ab.png");
        assert!(db.get_by_path(Path::new("/elsewhere/a_.png")).unwrap().is_none());
        assert!(db.get_by_path(Path::new("/elsewhere/%.png")).unwrap().is_none());
        assert_eq!(db.get_by_path(Path::new("/elsewhere/ab.png")).unwrap().unwrap().id, plain);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thumbnails_are_recorded_and_deleted_and_show_marks() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-th-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("s.png");
        image::RgbaImage::from_pixel(40, 40, image::Rgba([255, 255, 255, 255])).save(&png).unwrap();
        let mut st = crate::annot::EditState::default();
        st.marks.push(crate::annot::Mark::Fill {
            rect: crate::annot::NormRect::new(0.0, 0.0, 1.0, 1.0),
            colour: crate::annot::Colour::rgb(0.0, 0.0, 0.0),
        });
        st.save(&crate::annot::sidecar_path(&png)).unwrap();
        let db = open(&dir.join("capture.sqlite")).unwrap();
        let id = db.insert(&NewCapture { kind: CaptureKind::Screenshot, width: 40, height: 40, path: png.clone(), source: "grim".into() }).unwrap();
        let thumb = dir.join("thumbs").join(format!("{id}.jpg"));
        db.thumb(&id, &png, &thumb, false).unwrap();
        // The thumbnail is the redacted picture (black), not the original (white).
        assert!(image::open(&thumb).unwrap().to_luma8().get_pixel(5, 5)[0] < 40);
        assert_eq!(crate::export::shareable(&png).unwrap().to_luma8().get_pixel(5, 5)[0], 0);
        assert!(db.delete(&id).unwrap().contains(&thumb));
        assert!(!thumb.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thumb_is_a_smaller_jpeg() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-thumb-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("src.png");
        image::RgbImage::from_pixel(64, 32, image::Rgb([20, 80, 160])).save(&png).unwrap();
        let jpg = dir.join("t.jpg");
        write_thumb(&png, &jpg, 16).unwrap();
        let thumb = image::open(&jpg).unwrap();
        assert!(thumb.width() <= 16 && thumb.height() <= 16);
        write_thumb(&png, &jpg, 8).unwrap();
        let again = image::open(&jpg).unwrap();
        assert_eq!(again.width(), thumb.width());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insert_png_stores_a_dhash() {
        let dir = std::env::temp_dir().join(format!("nyx-capture-dhash-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(16, 8, |x, _| image::Rgb([x as u8 * 16, 0, 0])))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let db = open(&dir.join("capture.sqlite")).unwrap();
        let (id, _) = db.insert_png(&dir, &png, 16, 8, "grim").unwrap();
        let hash = db.dhash(&id).unwrap().expect("dhash");
        assert_ne!(hash, 0);
        assert_eq!(hash, dhash(&image::load_from_memory(&png).unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dhash_matches_identical_pictures() {
        let a = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(32, 16, |x, y| image::Rgb([x as u8, y as u8, 40])));
        let b = a.clone();
        let other = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(32, 16, |x, y| image::Rgb([y as u8, x as u8, 40])));
        assert_eq!(dhash(&a), dhash(&b));
        assert_ne!(dhash(&a), dhash(&other));
    }
}

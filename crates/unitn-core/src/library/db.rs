use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, Connection, Row};

use crate::model::{Album, Artist, Playlist, Station, Track};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (path TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS tracks (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    mtime        INTEGER NOT NULL DEFAULT 0,
    title        TEXT NOT NULL DEFAULT '',
    artist       TEXT NOT NULL DEFAULT '',
    album        TEXT NOT NULL DEFAULT '',
    album_artist TEXT NOT NULL DEFAULT '',
    track_no     INTEGER NOT NULL DEFAULT 0,
    disc_no      INTEGER NOT NULL DEFAULT 0,
    year         INTEGER NOT NULL DEFAULT 0,
    genre        TEXT NOT NULL DEFAULT '',
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    cover        TEXT
);
CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album_artist, artist, album);
CREATE TABLE IF NOT EXISTS playlists (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL,
    created INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS playlist_items (
    playlist_id INTEGER NOT NULL,
    position    INTEGER NOT NULL,
    path        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_playlist_items ON playlist_items(playlist_id, position);
CREATE TABLE IF NOT EXISTS artist_images (
    artist  TEXT PRIMARY KEY,
    path    TEXT,
    checked INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS art_lookups (
    artist  TEXT NOT NULL,
    album   TEXT NOT NULL,
    found   INTEGER NOT NULL,
    checked INTEGER NOT NULL,
    PRIMARY KEY (artist, album)
);
CREATE TABLE IF NOT EXISTS album_fixes (
    orig_artist TEXT NOT NULL,
    orig_album  TEXT NOT NULL,
    artist      TEXT NOT NULL,
    album       TEXT NOT NULL,
    year        INTEGER NOT NULL DEFAULT 0,
    cover       TEXT,
    PRIMARY KEY (orig_artist, orig_album)
);
CREATE TABLE IF NOT EXISTS remote_tracks (
    path         TEXT PRIMARY KEY,
    title        TEXT NOT NULL DEFAULT '',
    artist       TEXT NOT NULL DEFAULT '',
    album        TEXT NOT NULL DEFAULT '',
    album_artist TEXT NOT NULL DEFAULT '',
    track_no     INTEGER NOT NULL DEFAULT 0,
    disc_no      INTEGER NOT NULL DEFAULT 0,
    year         INTEGER NOT NULL DEFAULT 0,
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    cover        TEXT,
    stream_ext   TEXT,
    stream_ref   TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS stations (
    uuid    TEXT PRIMARY KEY,
    name    TEXT NOT NULL,
    url     TEXT NOT NULL,
    favicon TEXT NOT NULL DEFAULT '',
    country TEXT NOT NULL DEFAULT '',
    tags    TEXT NOT NULL DEFAULT '',
    codec   TEXT NOT NULL DEFAULT '',
    bitrate INTEGER NOT NULL DEFAULT 0,
    added   INTEGER NOT NULL DEFAULT 0
);
";

/// The artist an album is grouped under: album artist, falling back to artist.
const GROUP_ARTIST: &str = "(CASE WHEN album_artist <> '' THEN album_artist ELSE artist END)";

const TRACK_COLS: &str =
    "id, path, title, artist, album, album_artist, track_no, disc_no, year, genre, duration_ms, cover";

/// SQLite-backed index of the user's music and favorite radio stations.
pub struct Library {
    conn: Connection,
}

fn track_from_row(r: &Row) -> rusqlite::Result<Track> {
    Ok(Track {
        id: r.get(0)?,
        path: PathBuf::from(r.get::<_, String>(1)?),
        title: r.get(2)?,
        artist: r.get(3)?,
        album: r.get(4)?,
        album_artist: r.get(5)?,
        track_no: r.get::<_, i64>(6)? as u32,
        disc_no: r.get::<_, i64>(7)? as u32,
        year: r.get::<_, i64>(8)? as u32,
        genre: r.get(9)?,
        duration_ms: r.get::<_, i64>(10)?.max(0) as u64,
        cover: r.get::<_, Option<String>>(11)?.map(PathBuf::from),
        ..Default::default()
    })
}

const REMOTE_COLS: &str =
    "path, title, artist, album, album_artist, track_no, disc_no, year, duration_ms, cover, stream_ext";

fn remote_from_row(r: &Row) -> rusqlite::Result<Track> {
    Ok(Track {
        path: PathBuf::from(r.get::<_, String>(0)?),
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        album_artist: r.get(4)?,
        track_no: r.get::<_, i64>(5)? as u32,
        disc_no: r.get::<_, i64>(6)? as u32,
        year: r.get::<_, i64>(7)? as u32,
        duration_ms: r.get::<_, i64>(8)?.max(0) as u64,
        cover: r.get::<_, Option<String>>(9)?.map(PathBuf::from),
        stream_ext: r.get(10)?,
        ..Default::default()
    })
}

fn album_from_row(r: &Row) -> rusqlite::Result<Album> {
    Ok(Album {
        artist: r.get(0)?,
        title: r.get(1)?,
        year: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
        track_count: r.get::<_, i64>(3)? as u32,
        cover: r.get::<_, Option<String>>(4)?.map(PathBuf::from),
        duration_ms: r.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u64,
    })
}

fn station_from_row(r: &Row) -> rusqlite::Result<Station> {
    Ok(Station {
        uuid: r.get(0)?,
        name: r.get(1)?,
        url: r.get(2)?,
        favicon: r.get(3)?,
        country: r.get(4)?,
        tags: r.get(5)?,
        codec: r.get(6)?,
        bitrate: r.get::<_, i64>(7)?.max(0) as u32,
    })
}

struct AlbumFix {
    orig_artist: String,
    artist: String,
    album: String,
    year: u32,
    cover: Option<PathBuf>,
}

impl AlbumFix {
    fn apply(&self, t: &mut Track) {
        if t.artist == self.orig_artist || t.artist.is_empty() {
            t.artist = self.artist.clone();
        }
        t.album_artist = self.artist.clone();
        t.album = self.album.clone();
        if self.year > 0 {
            t.year = self.year;
        }
        if self.cover.is_some() {
            t.cover = self.cover.clone();
        }
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(10))?;
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub(crate) fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    pub(crate) fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub(crate) fn rollback(&self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }

    // ---- folders -------------------------------------------------------

    pub fn folders(&self) -> Result<Vec<PathBuf>> {
        let mut st = self.conn.prepare("SELECT path FROM folders ORDER BY path")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).map(PathBuf::from).collect())
    }

    pub fn add_folder(&self, folder: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO folders(path) VALUES (?1)",
            params![path_str(folder)],
        )?;
        Ok(())
    }

    /// Removes a folder and every track indexed below it.
    pub fn remove_folder(&self, folder: &Path) -> Result<()> {
        let s = path_str(folder);
        self.conn.execute("DELETE FROM folders WHERE path = ?1", params![s])?;
        let mut st = self.conn.prepare("SELECT path FROM tracks")?;
        let paths: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        for p in paths {
            if Path::new(&p).starts_with(folder) {
                self.conn.execute("DELETE FROM tracks WHERE path = ?1", params![p])?;
            }
        }
        Ok(())
    }

    // ---- tracks --------------------------------------------------------

    pub fn track_mtimes(&self) -> Result<HashMap<PathBuf, i64>> {
        let mut st = self.conn.prepare("SELECT path, mtime FROM tracks")?;
        let rows = st.query_map([], |r| Ok((PathBuf::from(r.get::<_, String>(0)?), r.get::<_, i64>(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn upsert_track(&self, t: &Track, mtime: i64) -> Result<()> {
        // Corrections made with "Fix album info" outlive rescans.
        let mut fixed;
        let t = match self.album_fix(t.grouping_artist(), &t.album)? {
            Some(fix) => {
                fixed = t.clone();
                fix.apply(&mut fixed);
                &fixed
            }
            None => t,
        };
        self.conn.execute(
            "INSERT INTO tracks (path, mtime, title, artist, album, album_artist, track_no, disc_no, year, genre, duration_ms, cover)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(path) DO UPDATE SET
                mtime = excluded.mtime, title = excluded.title, artist = excluded.artist,
                album = excluded.album, album_artist = excluded.album_artist,
                track_no = excluded.track_no, disc_no = excluded.disc_no, year = excluded.year,
                genre = excluded.genre, duration_ms = excluded.duration_ms, cover = excluded.cover",
            params![
                path_str(&t.path),
                mtime,
                t.title,
                t.artist,
                t.album,
                t.album_artist,
                t.track_no as i64,
                t.disc_no as i64,
                t.year as i64,
                t.genre,
                t.duration_ms as i64,
                t.cover.as_deref().map(path_str),
            ],
        )?;
        Ok(())
    }

    pub fn delete_track(&self, path: &Path) -> Result<()> {
        self.conn.execute("DELETE FROM tracks WHERE path = ?1", params![path_str(path)])?;
        Ok(())
    }

    pub fn track_count(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// All tracks, or those whose title/artist/album contain `query`.
    pub fn tracks(&self, query: Option<&str>) -> Result<Vec<Track>> {
        let order = "ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, disc_no, track_no, title COLLATE NOCASE";
        let q = query.map(str::trim).filter(|q| !q.is_empty());
        let rows = match q {
            Some(q) => {
                let sql = format!(
                    "SELECT {TRACK_COLS} FROM tracks
                     WHERE title LIKE ?1 ESCAPE '\\' OR artist LIKE ?1 ESCAPE '\\'
                        OR album LIKE ?1 ESCAPE '\\' OR album_artist LIKE ?1 ESCAPE '\\' {order}"
                );
                let mut st = self.conn.prepare(&sql)?;
                let rows = st
                    .query_map(params![like_pattern(q)], track_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            }
            None => {
                let sql = format!("SELECT {TRACK_COLS} FROM tracks {order}");
                let mut st = self.conn.prepare(&sql)?;
                let rows = st.query_map([], track_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            }
        };
        Ok(rows)
    }

    /// Albums, optionally only those of one (grouping) artist.
    pub fn albums(&self, artist: Option<&str>) -> Result<Vec<Album>> {
        let filter = if artist.is_some() {
            format!("WHERE {GROUP_ARTIST} = ?1")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT {GROUP_ARTIST} AS ga, album, MAX(year), COUNT(*), MAX(cover), SUM(duration_ms)
             FROM tracks {filter}
             GROUP BY ga, album
             ORDER BY ga COLLATE NOCASE, MAX(year), album COLLATE NOCASE"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = match artist {
            Some(a) => st.query_map(params![a], album_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?,
            None => st.query_map([], album_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn album_tracks(&self, artist: &str, album: &str) -> Result<Vec<Track>> {
        let sql = format!(
            "SELECT {TRACK_COLS} FROM tracks WHERE {GROUP_ARTIST} = ?1 AND album = ?2
             ORDER BY disc_no, track_no, title COLLATE NOCASE"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map(params![artist, album], track_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn artists(&self) -> Result<Vec<Artist>> {
        let sql = format!(
            "SELECT {GROUP_ARTIST} AS ga, COUNT(DISTINCT album), COUNT(*) FROM tracks
             GROUP BY ga ORDER BY ga COLLATE NOCASE"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map([], |r| {
                Ok(Artist {
                    name: r.get(0)?,
                    album_count: r.get::<_, i64>(1)? as u32,
                    track_count: r.get::<_, i64>(2)? as u32,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- playlists -------------------------------------------------------

    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut st = self.conn.prepare(
            "SELECT p.id, p.name, (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id)
             FROM playlists p ORDER BY p.name COLLATE NOCASE, p.id",
        )?;
        let rows = st
            .query_map([], |r| Ok(Playlist { id: r.get(0)?, name: r.get(1)?, track_count: r.get::<_, i64>(2)? as u32 }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO playlists (name, created) VALUES (?1, CAST(strftime('%s','now') AS INTEGER))",
            params![name],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE playlists SET name = ?1 WHERE id = ?2", params![name, id])?;
        Ok(())
    }

    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// File paths of a playlist, in order.
    pub fn playlist_paths(&self, id: i64) -> Result<Vec<PathBuf>> {
        let mut st =
            self.conn.prepare("SELECT path FROM playlist_items WHERE playlist_id = ?1 ORDER BY position")?;
        let rows = st
            .query_map(params![id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().map(PathBuf::from).collect())
    }

    /// Tracks of a playlist, in order. Files that are not in the library
    /// (e.g. imported from an M3U) appear with their file name as title.
    pub fn playlist_tracks(&self, id: i64) -> Result<Vec<Track>> {
        let sql = format!("SELECT {TRACK_COLS} FROM tracks WHERE path = ?1");
        let mut st = self.conn.prepare(&sql)?;
        let mut remote = self.conn.prepare(&format!("SELECT {REMOTE_COLS} FROM remote_tracks WHERE path = ?1"))?;
        let mut out = Vec::new();
        for path in self.playlist_paths(id)? {
            let p = path_str(&path);
            let mut found = st.query_map(params![p], track_from_row)?.next().transpose()?;
            if found.is_none() {
                found = remote.query_map(params![p], remote_from_row)?.next().transpose()?;
            }
            out.push(found.unwrap_or_else(|| Track {
                title: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                artist: "Unknown Artist".into(),
                album: "Unknown Album".into(),
                path: path.clone(),
                ..Default::default()
            }));
        }
        Ok(out)
    }

    /// Replaces the contents of a playlist.
    pub fn set_playlist_paths(&self, id: i64, paths: &[PathBuf]) -> Result<()> {
        self.conn.execute_batch("SAVEPOINT set_playlist")?;
        let result = (|| -> Result<()> {
            self.conn.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", params![id])?;
            let mut st =
                self.conn.prepare("INSERT INTO playlist_items (playlist_id, position, path) VALUES (?1, ?2, ?3)")?;
            for (i, p) in paths.iter().enumerate() {
                st.execute(params![id, i as i64, path_str(p)])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute_batch("RELEASE set_playlist")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK TO set_playlist; RELEASE set_playlist");
                Err(e)
            }
        }
    }

    /// Remembers server songs (with the credential-free part of their stream
    /// address) so playlists can show and play them later.
    pub fn remember_remote_tracks(&self, tracks: &[(Track, String)]) -> Result<()> {
        let mut st = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO remote_tracks
             (path, title, artist, album, album_artist, track_no, disc_no, year, duration_ms, cover, stream_ext, stream_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for (t, stream_ref) in tracks {
            st.execute(params![
                path_str(&t.path),
                t.title,
                t.artist,
                t.album,
                t.album_artist,
                t.track_no as i64,
                t.disc_no as i64,
                t.year as i64,
                t.duration_ms as i64,
                t.cover.as_deref().map(path_str),
                t.stream_ext,
                stream_ref,
            ])?;
        }
        Ok(())
    }

    /// The saved stream reference of a remembered server song.
    pub fn remote_stream_ref(&self, path: &Path) -> Result<Option<String>> {
        let mut st = self.conn.prepare_cached("SELECT stream_ref FROM remote_tracks WHERE path = ?1")?;
        Ok(st.query_map(params![path_str(path)], |r| r.get::<_, String>(0))?.next().transpose()?)
    }

    pub fn add_to_playlist(&self, id: i64, paths: &[PathBuf]) -> Result<()> {
        let mut all = self.playlist_paths(id)?;
        all.extend_from_slice(paths);
        self.set_playlist_paths(id, &all)
    }

    pub fn remove_from_playlist(&self, id: i64, index: usize) -> Result<()> {
        let mut all = self.playlist_paths(id)?;
        if index < all.len() {
            all.remove(index);
            self.set_playlist_paths(id, &all)?;
        }
        Ok(())
    }

    pub fn move_in_playlist(&self, id: i64, from: usize, to: usize) -> Result<()> {
        let mut all = self.playlist_paths(id)?;
        if from < all.len() && to < all.len() && from != to {
            let item = all.remove(from);
            all.insert(to, item);
            self.set_playlist_paths(id, &all)?;
        }
        Ok(())
    }

    // ---- online album art -----------------------------------------------

    /// Albums without any cover that were not looked up online yet.
    pub fn albums_missing_art(&self) -> Result<Vec<(String, String)>> {
        let sql = format!(
            "SELECT {GROUP_ARTIST} AS ga, album FROM tracks
             GROUP BY ga, album
             HAVING MAX(cover) IS NULL
                AND ga <> 'Unknown Artist' AND album <> 'Unknown Album'
                AND NOT EXISTS (SELECT 1 FROM art_lookups l
                     WHERE l.artist = (CASE WHEN tracks.album_artist <> '' THEN tracks.album_artist ELSE tracks.artist END)
                       AND l.album = tracks.album)
             ORDER BY ga COLLATE NOCASE, album COLLATE NOCASE"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Remembers an online lookup so it is not repeated on every scan.
    pub fn record_art_lookup(&self, artist: &str, album: &str, found: bool) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO art_lookups (artist, album, found, checked)
             VALUES (?1, ?2, ?3, CAST(strftime('%s','now') AS INTEGER))",
            params![artist, album, found as i64],
        )?;
        Ok(())
    }

    /// Sets the cover of every track of an album that has none.
    pub fn set_album_cover(&self, artist: &str, album: &str, cover: &Path) -> Result<()> {
        let sql = format!("UPDATE tracks SET cover = ?1 WHERE {GROUP_ARTIST} = ?2 AND album = ?3 AND cover IS NULL");
        self.conn.execute(&sql, params![path_str(cover), artist, album])?;
        Ok(())
    }

    fn album_fix(&self, artist: &str, album: &str) -> Result<Option<AlbumFix>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT orig_artist, artist, album, year, cover FROM album_fixes WHERE orig_artist = ?1 AND orig_album = ?2")?;
        let mut rows = st.query(params![artist, album])?;
        Ok(match rows.next()? {
            Some(r) => Some(AlbumFix {
                orig_artist: r.get(0)?,
                artist: r.get(1)?,
                album: r.get(2)?,
                year: r.get::<_, i64>(3)?.max(0) as u32,
                cover: r.get::<_, Option<String>>(4)?.map(PathBuf::from),
            }),
            None => None,
        })
    }

    /// Corrects an album's artist, title, year (0 = keep) and cover (`None` =
    /// keep). The correction is remembered, so rescanning the files keeps it.
    pub fn fix_album(
        &self,
        artist: &str,
        album: &str,
        new_artist: &str,
        new_album: &str,
        year: u32,
        cover: Option<&Path>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        // Albums that were already corrected into this one keep pointing at it.
        let mut origins: Vec<(String, String, u32, Option<String>)> = {
            let mut st = tx.prepare("SELECT orig_artist, orig_album, year, cover FROM album_fixes WHERE artist = ?1 AND album = ?2")?;
            let rows = st.query_map(params![artist, album], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u32, r.get(3)?)))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        if !origins.iter().any(|o| o.0 == artist && o.1 == album) {
            origins.push((artist.to_string(), album.to_string(), 0, None));
        }
        for (orig_artist, orig_album, old_year, old_cover) in origins {
            tx.execute(
                "INSERT OR REPLACE INTO album_fixes (orig_artist, orig_album, artist, album, year, cover)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    orig_artist,
                    orig_album,
                    new_artist,
                    new_album,
                    if year > 0 { year } else { old_year } as i64,
                    cover.map(path_str).or(old_cover),
                ],
            )?;
        }
        tx.execute(
            &format!(
                "UPDATE tracks SET
                    artist = CASE WHEN artist = ?1 THEN ?3 ELSE artist END,
                    album_artist = ?3, album = ?4,
                    year = CASE WHEN ?5 > 0 THEN ?5 ELSE year END,
                    cover = COALESCE(?6, cover)
                 WHERE {GROUP_ARTIST} = ?1 AND album = ?2"
            ),
            params![artist, album, new_artist, new_album, year as i64, cover.map(path_str)],
        )?;
        // No automatic lookup for the corrected album: the user chose.
        tx.execute(
            "INSERT OR REPLACE INTO art_lookups (artist, album, found, checked)
             VALUES (?1, ?2, 1, CAST(strftime('%s','now') AS INTEGER))",
            params![new_artist, new_album],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Cached artist photo: `None` = never looked up, `Some(None)` = none found.
    pub fn artist_image(&self, artist: &str) -> Result<Option<Option<PathBuf>>> {
        let mut st = self.conn.prepare("SELECT path FROM artist_images WHERE artist = ?1")?;
        let row = st.query_map(params![artist], |r| r.get::<_, Option<String>>(0))?.next().transpose()?;
        Ok(row.map(|p| p.map(PathBuf::from)))
    }

    pub fn set_artist_image(&self, artist: &str, path: Option<&Path>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO artist_images (artist, path, checked)
             VALUES (?1, ?2, CAST(strftime('%s','now') AS INTEGER))",
            params![artist, path.map(path_str)],
        )?;
        Ok(())
    }

    // ---- favorite stations ----------------------------------------------

    pub fn favorite_stations(&self) -> Result<Vec<Station>> {
        let mut st = self.conn.prepare(
            "SELECT uuid, name, url, favicon, country, tags, codec, bitrate FROM stations ORDER BY added DESC, name",
        )?;
        let rows = st.query_map([], station_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn favorite_uuids(&self) -> Result<HashSet<String>> {
        let mut st = self.conn.prepare("SELECT uuid FROM stations")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn add_favorite_station(&self, s: &Station) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO stations (uuid, name, url, favicon, country, tags, codec, bitrate, added)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CAST(strftime('%s','now') AS INTEGER))",
            params![s.uuid, s.name, s.url, s.favicon, s.country, s.tags, s.codec, s.bitrate as i64],
        )?;
        Ok(())
    }

    pub fn remove_favorite_station(&self, uuid: &str) -> Result<()> {
        self.conn.execute("DELETE FROM stations WHERE uuid = ?1", params![uuid])?;
        Ok(())
    }
}

/// Builds a `LIKE` pattern matching `q` anywhere, escaping wildcards.
fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str, title: &str, artist: &str, album: &str, no: u32) -> Track {
        Track {
            path: PathBuf::from(path),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            track_no: no,
            duration_ms: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn playlists_hold_server_songs() {
        let lib = Library::open_in_memory().unwrap();
        lib.upsert_track(&track("/m/a/1.mp3", "Local", "Band", "Album", 1), 1).unwrap();
        let remote = Track {
            path: PathBuf::from("jellyfin://nas:8096/T1"),
            title: "From the server".into(),
            artist: "Server Band".into(),
            album: "Server Album".into(),
            duration_ms: 5000,
            stream_ext: Some("flac".into()),
            stream_url: Some("http://nas:8096/Audio/T1/stream?static=true&api_key=SECRET".into()),
            ..Default::default()
        };
        lib.remember_remote_tracks(&[(remote.clone(), "/Audio/T1/stream?static=true".into())]).unwrap();
        let id = lib.create_playlist("Mix").unwrap();
        lib.add_to_playlist(id, &[PathBuf::from("/m/a/1.mp3"), remote.path.clone()]).unwrap();

        let tracks = lib.playlist_tracks(id).unwrap();
        assert_eq!(tracks[0].title, "Local");
        assert_eq!(tracks[1].title, "From the server");
        assert_eq!(tracks[1].album, "Server Album");
        assert_eq!(tracks[1].stream_ext.as_deref(), Some("flac"));
        assert_eq!(tracks[1].stream_url, None);
        assert_eq!(lib.remote_stream_ref(&remote.path).unwrap().as_deref(), Some("/Audio/T1/stream?static=true"));
    }

    #[test]
    fn indexes_and_groups_tracks() {
        let lib = Library::open_in_memory().unwrap();
        lib.upsert_track(&track("/m/a/2.mp3", "Second", "Band", "First Album", 2), 1).unwrap();
        lib.upsert_track(&track("/m/a/1.mp3", "First", "Band", "First Album", 1), 1).unwrap();
        lib.upsert_track(&track("/m/b/1.mp3", "Solo", "Singer", "Debut", 1), 1).unwrap();

        let albums = lib.albums(None).unwrap();
        assert_eq!(albums.len(), 2);
        assert_eq!(albums[0].title, "First Album");
        assert_eq!(albums[0].track_count, 2);

        let tracks = lib.album_tracks("Band", "First Album").unwrap();
        assert_eq!(tracks[0].title, "First");

        assert_eq!(lib.artists().unwrap().len(), 2);
        assert_eq!(lib.tracks(Some("solo")).unwrap().len(), 1);
        assert_eq!(lib.tracks(Some("100%")).unwrap().len(), 0);

        // Updating keeps one row per path.
        lib.upsert_track(&track("/m/b/1.mp3", "Solo (Remastered)", "Singer", "Debut", 1), 2).unwrap();
        assert_eq!(lib.track_count().unwrap(), 3);
    }

    #[test]
    fn playlists_keep_order_and_survive_missing_files() {
        let lib = Library::open_in_memory().unwrap();
        lib.upsert_track(&track("/m/a/1.mp3", "One", "Band", "Album", 1), 1).unwrap();
        lib.upsert_track(&track("/m/a/2.mp3", "Two", "Band", "Album", 2), 1).unwrap();
        let id = lib.create_playlist("Road trip").unwrap();
        lib.add_to_playlist(id, &[PathBuf::from("/m/a/2.mp3"), PathBuf::from("/m/a/1.mp3")]).unwrap();
        lib.add_to_playlist(id, &[PathBuf::from("/elsewhere/Extra Song.flac")]).unwrap();

        let titles: Vec<String> = lib.playlist_tracks(id).unwrap().into_iter().map(|t| t.title).collect();
        assert_eq!(titles, ["Two", "One", "Extra Song"]);
        assert_eq!(lib.playlists().unwrap()[0].track_count, 3);

        lib.move_in_playlist(id, 2, 0).unwrap();
        lib.remove_from_playlist(id, 2).unwrap();
        let titles: Vec<String> = lib.playlist_tracks(id).unwrap().into_iter().map(|t| t.title).collect();
        assert_eq!(titles, ["Extra Song", "Two"]);

        lib.rename_playlist(id, "Summer").unwrap();
        assert_eq!(lib.playlists().unwrap()[0].name, "Summer");
        lib.delete_playlist(id).unwrap();
        assert!(lib.playlists().unwrap().is_empty());
        assert!(lib.playlist_paths(id).unwrap().is_empty());
    }

    #[test]
    fn tracks_albums_missing_art() {
        let lib = Library::open_in_memory().unwrap();
        lib.upsert_track(&track("/m/a/1.mp3", "A", "Band", "Album One", 1), 1).unwrap();
        lib.upsert_track(&track("/m/u/1.mp3", "B", "Unknown Artist", "Unknown Album", 1), 1).unwrap();
        let mut with_cover = track("/m/c/1.mp3", "C", "Band", "Has Art", 1);
        with_cover.cover = Some(PathBuf::from("/covers/x.jpg"));
        lib.upsert_track(&with_cover, 1).unwrap();

        assert_eq!(lib.albums_missing_art().unwrap(), vec![("Band".to_string(), "Album One".to_string())]);
        lib.record_art_lookup("Band", "Album One", false).unwrap();
        assert!(lib.albums_missing_art().unwrap().is_empty());

        lib.set_album_cover("Band", "Album One", Path::new("/covers/y.jpg")).unwrap();
        assert_eq!(lib.album_tracks("Band", "Album One").unwrap()[0].cover, Some(PathBuf::from("/covers/y.jpg")));
    }

    #[test]
    fn folders_and_favorites() {
        let lib = Library::open_in_memory().unwrap();
        lib.add_folder(Path::new("/m")).unwrap();
        lib.upsert_track(&track("/m/a/1.mp3", "A", "B", "C", 1), 1).unwrap();
        lib.upsert_track(&track("/other/1.mp3", "D", "E", "F", 1), 1).unwrap();
        lib.remove_folder(Path::new("/m")).unwrap();
        assert!(lib.folders().unwrap().is_empty());
        assert_eq!(lib.track_count().unwrap(), 1);

        let s = Station { uuid: "u1".into(), name: "Jazz FM".into(), url: "http://x".into(), ..Default::default() };
        lib.add_favorite_station(&s).unwrap();
        assert!(lib.favorite_uuids().unwrap().contains("u1"));
        assert_eq!(lib.favorite_stations().unwrap()[0].name, "Jazz FM");
        lib.remove_favorite_station("u1").unwrap();
        assert!(lib.favorite_stations().unwrap().is_empty());
    }

    #[test]
    fn album_fixes_survive_rescans() {
        let lib = Library::open_in_memory().unwrap();
        let a = track("/m/1.mp3", "One", "Nine Inch Nail", "Ghost", 1);
        let b = track("/m/2.mp3", "Two", "Nine Inch Nail", "Ghost", 2);
        lib.upsert_track(&a, 1).unwrap();
        lib.upsert_track(&b, 1).unwrap();
        lib.fix_album("Nine Inch Nail", "Ghost", "Nine Inch Nails", "Ghosts I–IV", 2008, Some(Path::new("/c/x.jpg"))).unwrap();

        let albums = lib.albums(None).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "Nine Inch Nails");
        assert_eq!(albums[0].title, "Ghosts I–IV");
        assert_eq!(albums[0].year, 2008);
        assert_eq!(albums[0].cover.as_deref(), Some(Path::new("/c/x.jpg")));

        // A rescan reads the old tags again; the correction still applies.
        lib.upsert_track(&a, 2).unwrap();
        let t = lib.album_tracks("Nine Inch Nails", "Ghosts I–IV").unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].artist, "Nine Inch Nails");

        // Correcting again follows the chain back to the original tags.
        lib.fix_album("Nine Inch Nails", "Ghosts I–IV", "NIN", "Ghosts", 0, None).unwrap();
        lib.upsert_track(&b, 3).unwrap();
        let albums = lib.albums(None).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!((albums[0].artist.as_str(), albums[0].title.as_str(), albums[0].year), ("NIN", "Ghosts", 2008));
        assert_eq!(albums[0].cover.as_deref(), Some(Path::new("/c/x.jpg")));
    }
}

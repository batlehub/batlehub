//! RFC 0048 §15 Q9 — the detached single-flight cold fill.
//!
//! A cold miss used to store the whole upstream body before serving a byte,
//! so a 475 MB archive behind a slow vendor link outlived the client's read
//! timeout, and each retry started another upstream fetch. Now the fill is a
//! task keyed on the storage key, spawned detached from the request:
//!
//! - the first miss starts it; every concurrent or retried request for the
//!   same key **joins** it and is served the bytes already received, then the
//!   rest as they arrive;
//! - a client that disconnects drops its subscription, never the fill;
//! - the newest chunk is always **withheld** until the fill ends, so a client
//!   that sees a complete body saw one whose digest was checked; on a
//!   mismatch, an oversize body or an upstream error the stream ends in an
//!   error instead (the response is reset) and nothing is promoted.
//!
//! Subscribers read the bytes back from a scratch file in the temp directory
//! rather than from memory, so a 1 GB fill holds one chunk in RAM, not 1 GB.
//! When the scratch file cannot be created the fill still runs and is still
//! shared, but serves nothing until it ends — store-then-serve, as before.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use crate::error::CoreError;
use crate::ports::ByteStream;

/// How a fill ended, as every subscriber needs to know it.
#[derive(Clone, Debug)]
pub(super) enum FillEnd {
    /// Stored, verified and promoted: the whole body may be served.
    Done,
    /// Upstream said `no-store`: nothing was cached and nothing is shared;
    /// a joiner fetches for itself.
    NoStore,
    /// The fill failed; every subscriber gets this error.
    Failed(FillError),
}

/// A `CoreError` every subscriber can be handed a copy of.
#[derive(Clone, Debug)]
pub(super) struct FillError {
    kind: ErrorKind,
    message: String,
}

#[derive(Clone, Copy, Debug)]
enum ErrorKind {
    NotFound,
    TooLarge,
    Integrity,
    Denied,
    Other,
}

impl From<&CoreError> for FillError {
    fn from(e: &CoreError) -> Self {
        let kind = match e {
            CoreError::NotFound(_) | CoreError::NotFoundWithheld(_) => ErrorKind::NotFound,
            CoreError::PayloadTooLarge(_) => ErrorKind::TooLarge,
            CoreError::IntegrityFailure(_) => ErrorKind::Integrity,
            CoreError::AccessDenied(_) => ErrorKind::Denied,
            _ => ErrorKind::Other,
        };
        let message = match e {
            CoreError::AccessDenied(d) => d.message.clone(),
            CoreError::NotFound(m)
            | CoreError::NotFoundWithheld(m)
            | CoreError::PayloadTooLarge(m)
            | CoreError::IntegrityFailure(m)
            | CoreError::Registry(m) => m.clone(),
            other => other.to_string(),
        };
        Self { kind, message }
    }
}

impl FillError {
    pub(super) fn to_core(&self) -> CoreError {
        let m = self.message.clone();
        match self.kind {
            ErrorKind::NotFound => CoreError::NotFound(m),
            ErrorKind::TooLarge => CoreError::PayloadTooLarge(m),
            ErrorKind::Integrity => CoreError::IntegrityFailure(m),
            ErrorKind::Denied => CoreError::access_denied(m),
            ErrorKind::Other => CoreError::Registry(m),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Progress {
    /// Bytes a subscriber may read from the scratch file: everything received
    /// except the newest chunk, until the fill ends.
    readable: u64,
    /// Whether the scratch file exists and is being written.
    spooled: bool,
    pub(super) end: Option<FillEnd>,
}

/// One running fill, shared by every request for its key.
pub(super) struct Fill {
    scratch: PathBuf,
    progress: watch::Sender<Progress>,
}

impl Drop for Fill {
    fn drop(&mut self) {
        // Subscribers already reading hold their own descriptor; unlinking
        // does not cut them off.
        let _ = std::fs::remove_file(&self.scratch);
    }
}

/// The running fills of one process, keyed on the artifact's storage key.
#[derive(Default)]
pub struct FillCoordinator {
    inflight: Mutex<HashMap<String, Arc<Fill>>>,
}

pub(super) enum Joined {
    /// This request starts the fill and must drive the writer to its end.
    Started(FillWriter),
    /// A fill for the key is already running.
    Joined(Arc<Fill>),
}

impl FillCoordinator {
    pub(super) fn join_or_start(self: &Arc<Self>, key: &str) -> Joined {
        let mut inflight = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(fill) = inflight.get(key) {
            return Joined::Joined(Arc::clone(fill));
        }
        let scratch = std::env::temp_dir().join(format!("batlehub-fill-{}", uuid::Uuid::new_v4()));
        let (progress, _) = watch::channel(Progress::default());
        let fill = Arc::new(Fill { scratch, progress });
        inflight.insert(key.to_owned(), Arc::clone(&fill));
        Joined::Started(FillWriter {
            fill,
            key: key.to_owned(),
            coordinator: Arc::clone(self),
            spool: Default::default(),
            ended: false,
        })
    }

    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        self.inflight.lock().map(|m| m.is_empty()).unwrap_or(false)
    }

    fn release(&self, key: &str, fill: &Arc<Fill>) {
        let mut inflight = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if inflight.get(key).is_some_and(|f| Arc::ptr_eq(f, fill)) {
            inflight.remove(key);
        }
    }
}

/// The starting request's handle on its fill: spools what upstream sends,
/// then says how it ended. Dropped without an end, the fill fails rather than
/// leaving its subscribers waiting forever.
pub(super) struct FillWriter {
    fill: Arc<Fill>,
    key: String,
    coordinator: Arc<FillCoordinator>,
    spool: Arc<tokio::sync::Mutex<Spool>>,
    ended: bool,
}

#[derive(Default)]
struct Spool {
    file: Option<tokio::fs::File>,
    failed: bool,
    total: u64,
}

impl FillWriter {
    pub(super) fn fill(&self) -> Arc<Fill> {
        Arc::clone(&self.fill)
    }

    /// Pass `stream` through, copying each chunk to the scratch file and
    /// making every chunk but the newest readable.
    pub(super) fn spool(&self, stream: ByteStream) -> ByteStream {
        use futures::StreamExt;
        let fill = Arc::clone(&self.fill);
        let spool = Arc::clone(&self.spool);
        Box::pin(stream.then(move |chunk| {
            let (fill, spool) = (Arc::clone(&fill), Arc::clone(&spool));
            async move {
                if let Ok(bytes) = &chunk {
                    spool.lock().await.append(&fill, bytes).await;
                }
                chunk
            }
        }))
    }

    pub(super) fn finish(mut self, end: FillEnd) {
        self.end(end);
    }

    fn end(&mut self, end: FillEnd) {
        if self.ended {
            return;
        }
        self.ended = true;
        // The writer task is the only other holder, and it has finished with
        // the stream by the time the fill ends; a busy lock means it has not,
        // and then nothing is made readable.
        let (spooled, total) = match self.spool.try_lock() {
            Ok(s) => (!s.failed && s.file.is_some(), s.total),
            Err(_) => (false, 0),
        };
        self.fill.progress.send_modify(|p| {
            if matches!(end, FillEnd::Done) && spooled {
                p.readable = total;
            }
            p.spooled = spooled;
            p.end = Some(end);
        });
        self.coordinator.release(&self.key, &self.fill);
    }
}

impl Spool {
    async fn append(&mut self, fill: &Fill, chunk: &Bytes) {
        if self.failed {
            return;
        }
        if self.file.is_none() {
            match tokio::fs::File::create(&fill.scratch).await {
                Ok(f) => self.file = Some(f),
                Err(e) => {
                    tracing::warn!(path = %fill.scratch.display(), error = %e, "cold fill: no scratch file, serving after the fill ends");
                    self.failed = true;
                    return;
                }
            }
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let written = async {
            file.write_all(chunk).await?;
            file.flush().await
        }
        .await;
        if let Err(e) = written {
            tracing::warn!(path = %fill.scratch.display(), error = %e, "cold fill: scratch write failed, serving after the fill ends");
            self.failed = true;
            fill.progress.send_modify(|p| p.spooled = false);
            return;
        }
        let before = self.total;
        self.total += chunk.len() as u64;
        fill.progress.send_modify(|p| {
            p.spooled = true;
            p.readable = before;
        });
    }
}

impl Drop for FillWriter {
    fn drop(&mut self) {
        self.end(FillEnd::Failed(FillError {
            kind: ErrorKind::Other,
            message: "the cold fill stopped before it finished".to_owned(),
        }));
    }
}

/// What a request does with a fill once it has one.
pub(super) enum Served {
    /// The bytes, streamed from the scratch file as they become readable.
    Stream(ByteStream),
    /// The fill ended and stored the artifact; read it from storage.
    FromStorage,
    /// Upstream forbade caching; fetch for this request alone.
    NoStore,
}

impl Fill {
    /// Wait until there is something to serve, then say how to serve it.
    ///
    /// `wait_for_end` is the quarantine case: a registry that holds artifacts
    /// before serving them never streams a byte the policy has not seen.
    pub(super) async fn serve(self: Arc<Self>, wait_for_end: bool) -> Result<Served, CoreError> {
        let mut rx = self.progress.subscribe();
        loop {
            let p = rx.borrow_and_update().clone();
            match &p.end {
                Some(FillEnd::Failed(e)) => return Err(e.to_core()),
                Some(FillEnd::NoStore) => return Ok(Served::NoStore),
                Some(FillEnd::Done) if !p.spooled || wait_for_end => {
                    return Ok(Served::FromStorage)
                }
                Some(FillEnd::Done) => return Ok(Served::Stream(self.stream(rx))),
                None if !wait_for_end && p.spooled && p.readable > 0 => {
                    return Ok(Served::Stream(self.stream(rx)))
                }
                None => {}
            }
            if rx.changed().await.is_err() {
                return Err(CoreError::Registry("the cold fill vanished".to_owned()));
            }
        }
    }

    fn stream(self: Arc<Self>, rx: watch::Receiver<Progress>) -> ByteStream {
        struct Tail {
            fill: Arc<Fill>,
            rx: watch::Receiver<Progress>,
            file: Option<tokio::fs::File>,
            pos: u64,
            done: bool,
        }
        let tail = Tail {
            fill: self,
            rx,
            file: None,
            pos: 0,
            done: false,
        };
        Box::pin(futures::stream::unfold(tail, |mut t| async move {
            if t.done {
                return None;
            }
            loop {
                let p = t.rx.borrow_and_update().clone();
                if let Some(FillEnd::Failed(e)) = &p.end {
                    t.done = true;
                    return Some((Err(e.to_core()), t));
                }
                if t.pos < p.readable {
                    if t.file.is_none() {
                        match tokio::fs::File::open(&t.fill.scratch).await {
                            Ok(f) => t.file = Some(f),
                            Err(e) => {
                                t.done = true;
                                return Some((
                                    Err(CoreError::Storage(format!("cold fill scratch: {e}"))),
                                    t,
                                ));
                            }
                        }
                    }
                    let want = (p.readable - t.pos).min(64 * 1024) as usize;
                    let mut buf = vec![0u8; want];
                    let file = t.file.as_mut()?;
                    match file.read(&mut buf).await {
                        Ok(0) => {}
                        Ok(n) => {
                            buf.truncate(n);
                            t.pos += n as u64;
                            return Some((Ok(Bytes::from(buf)), t));
                        }
                        Err(e) => {
                            t.done = true;
                            return Some((
                                Err(CoreError::Storage(format!("cold fill scratch: {e}"))),
                                t,
                            ));
                        }
                    }
                } else if matches!(p.end, Some(FillEnd::Done)) {
                    return None;
                }
                if t.rx.changed().await.is_err() {
                    t.done = true;
                    return Some((
                        Err(CoreError::Registry("the cold fill vanished".to_owned())),
                        t,
                    ));
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn chunks(parts: &[&'static str]) -> ByteStream {
        let parts: Vec<Result<Bytes, CoreError>> = parts
            .iter()
            .map(|p| Ok(Bytes::from_static(p.as_bytes())))
            .collect();
        Box::pin(futures::stream::iter(parts))
    }

    /// RFC 0048 §6.6: a registry that holds artifacts before serving them
    /// never streams; it is served from storage once the fill has ended.
    #[tokio::test]
    async fn a_quarantined_kind_still_stores_before_serving() {
        let coordinator = Arc::new(FillCoordinator::default());
        let Joined::Started(writer) = coordinator.join_or_start("k") else {
            panic!("the first request starts the fill")
        };
        let mut spooled = writer.spool(chunks(&["one-", "two-"]));
        while spooled.next().await.is_some() {}
        drop(spooled);
        let fill = writer.fill();
        let held = tokio::spawn(Arc::clone(&fill).serve(true));
        let streamed = Arc::clone(&fill).serve(false).await.unwrap();
        assert!(
            matches!(streamed, Served::Stream(_)),
            "an ordinary registry streams"
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!held.is_finished(), "a holding registry waits for the end");
        writer.finish(FillEnd::Done);
        assert!(matches!(held.await.unwrap().unwrap(), Served::FromStorage));
        assert!(coordinator.is_idle());
    }

    /// The newest chunk is withheld until the fill ends, and a writer dropped
    /// without an end fails its subscribers instead of stranding them.
    #[tokio::test]
    async fn the_last_chunk_waits_for_the_end_and_a_dropped_writer_fails() {
        let coordinator = Arc::new(FillCoordinator::default());
        let Joined::Started(writer) = coordinator.join_or_start("k") else {
            panic!("the first request starts the fill")
        };
        let Joined::Joined(fill) = coordinator.join_or_start("k") else {
            panic!("the second request joins")
        };
        let mut spooled = writer.spool(chunks(&["one-", "two"]));
        while spooled.next().await.is_some() {}
        drop(spooled);
        let Served::Stream(mut s) = fill.serve(false).await.unwrap() else {
            panic!("readable bytes stream")
        };
        assert_eq!(
            s.next().await.unwrap().unwrap(),
            Bytes::from_static(b"one-")
        );
        drop(writer); // no end: the subscriber must not hang
        let err = s.next().await.unwrap().expect_err("the fill failed");
        assert!(
            err.to_string().contains("stopped before it finished"),
            "{err}"
        );
    }
}

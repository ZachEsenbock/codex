use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

/// Distinguish between stdout and stderr for display purposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

/// One unit of streamed output, typically a single line without the trailing newline.
#[derive(Clone, Debug)]
pub struct OutputItem {
    pub kind: StreamKind,
    pub text: String,
}

/// A bounded ring buffer of output lines for scrollback.
#[derive(Debug)]
pub struct RingBuffer {
    cap: usize,
    buf: VecDeque<OutputItem>,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            cap: capacity.max(1),
            buf: VecDeque::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn push(&mut self, item: OutputItem) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(item);
    }

    pub fn snapshot(&self) -> Vec<OutputItem> {
        self.buf.iter().cloned().collect()
    }
}

/// Handle for per-agent output: keeps an in-memory ring and a broadcast channel for live updates.
#[derive(Clone, Debug)]
pub struct AgentStream {
    inner: Arc<Mutex<RingBuffer>>,
    tx: broadcast::Sender<OutputItem>,
}

impl AgentStream {
    pub fn new(capacity: usize) -> Self {
        // Large enough default to be useful without hogging memory.
        let (tx, _rx) = broadcast::channel::<OutputItem>(1024);
        Self {
            inner: Arc::new(Mutex::new(RingBuffer::new(capacity))),
            tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OutputItem> {
        self.tx.subscribe()
    }

    pub async fn push_line(&self, kind: StreamKind, line: &str) {
        let item = OutputItem {
            kind,
            text: line.to_string(),
        };
        {
            let mut g = self.inner.lock().await;
            g.push(item.clone());
        }
        let _ = self.tx.send(item);
    }

    pub async fn snapshot(&self) -> Vec<OutputItem> {
        let g = self.inner.lock().await;
        g.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ring_buffer_caps_and_orders() {
        let ring = AgentStream::new(3);
        ring.push_line(StreamKind::Stdout, "a").await;
        ring.push_line(StreamKind::Stderr, "b").await;
        ring.push_line(StreamKind::Stdout, "c").await;
        ring.push_line(StreamKind::Stderr, "d").await; // evicts "a"
        let snap = ring.snapshot().await;
        let texts: Vec<String> = snap.into_iter().map(|i| i.text).collect();
        assert_eq!(texts, vec!["b", "c", "d"]);
    }
}

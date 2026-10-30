use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

type Callback = Box<dyn Fn() + Send + Sync>;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static CALLBACKS: Mutex<Vec<Callback>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<Callback>> {
    CALLBACKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ask the process to wind down. Callbacks run once, on the first request.
pub fn request() {
    if REQUESTED.swap(true, Ordering::SeqCst) {
        return;
    }

    let callbacks = std::mem::take(&mut *lock());

    for f in callbacks {
        f();
    }
}

pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Register a callback for the first `request()`. Runs at once when a request already happened.
pub fn on_request(f: impl Fn() + Send + Sync + 'static) {
    let mut callbacks = lock();

    if REQUESTED.load(Ordering::SeqCst) {
        drop(callbacks);
        f();
        return;
    }

    callbacks.push(Box::new(f));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    // the state is global, so one test walks the whole lifecycle
    #[test]
    fn callbacks_run_once_and_late_ones_run_at_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        on_request(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });

        assert!(!requested());
        request();
        request();
        assert!(requested());
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let c = count.clone();
        on_request(move || {
            c.fetch_add(10, Ordering::SeqCst);
        });
        assert_eq!(count.load(Ordering::SeqCst), 11);
    }
}

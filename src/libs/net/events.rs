use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mlua::{Function, IntoLuaMulti, Lua};
use tokio::io::{AsyncRead, BufReader};
use tokio::sync::{Mutex, Notify};

use super::bytestream;
use crate::engine::VmScheduler;

pub struct Stop {
    stopped: AtomicBool,
    notify: Notify,
}

impl Stop {
    pub fn new() -> Arc<Self> {
        Arc::new(Stop {
            stopped: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        while !self.is_stopped() {
            let notified = self.notify.notified();
            if self.is_stopped() {
                break;
            }
            notified.await;
        }
    }
}

pub struct TaskGuard {
    sched: Rc<VmScheduler>,
}

impl TaskGuard {
    pub fn new(sched: &Rc<VmScheduler>) -> Self {
        sched.retain_task();
        TaskGuard {
            sched: sched.clone(),
        }
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.sched.release_task();
    }
}

pub fn spawn_task<F>(sched: &Rc<VmScheduler>, fut: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    let guard = TaskGuard::new(sched);
    tokio::task::spawn_local(async move {
        let _guard = guard;
        fut.await;
    });
}

pub fn spawn_handler(
    lua: &Lua,
    sched: &Rc<VmScheduler>,
    what: &'static str,
    cb: &Function,
    args: impl IntoLuaMulti + 'static,
) {
    let thread = match lua.create_thread(cb.clone()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lehua: net: {what} handler error: {}", crate::error::pretty(&e));
            return;
        }
    };
    let fut = match thread.into_async::<()>(args) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lehua: net: {what} handler error: {}", crate::error::pretty(&e));
            return;
        }
    };
    let guard = TaskGuard::new(sched);
    tokio::task::spawn_local(async move {
        let _guard = guard;
        if let Err(e) = fut.await {
            eprintln!("lehua: net: {what} handler error: {}", crate::error::pretty(&e));
        }
    });
}

pub struct CloseState {
    pub cb: RefCell<Option<Function>>,
    pub fired: Cell<bool>,
}

impl CloseState {
    pub fn new() -> Rc<Self> {
        Rc::new(CloseState {
            cb: RefCell::new(None),
            fired: Cell::new(false),
        })
    }
}

pub fn fire_close(
    lua: &Lua,
    sched: &Rc<VmScheduler>,
    what: &'static str,
    state: &Rc<CloseState>,
    args: impl IntoLuaMulti + 'static,
) {
    if state.fired.get() {
        return;
    }
    state.fired.set(true);
    let cb = state.cb.borrow().clone();
    if let Some(cb) = cb {
        spawn_handler(lua, sched, what, &cb, args);
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum ReadEvent {
    Chunk,
    Line,
}

pub fn spawn_read_pump<R>(
    lua: &Lua,
    sched: &Rc<VmScheduler>,
    what: &'static str,
    close_what: &'static str,
    reader: Rc<Mutex<BufReader<R>>>,
    stop: Arc<Stop>,
    close_state: Rc<CloseState>,
    mode: ReadEvent,
    cb: Function,
) where
    R: AsyncRead + Unpin + 'static,
{
    let lua = lua.clone();
    let sched2 = sched.clone();
    spawn_task(sched, async move {
        let err: Option<String> = loop {
            let step = tokio::select! {
                _ = stop.wait() => None,
                r = async {
                    let mut guard = reader.lock().await;
                    let out = match mode {
                        ReadEvent::Chunk => bytestream::read_some(&mut guard, 64 * 1024)
                            .await
                            .map(|b| if b.is_empty() { None } else { Some(b) }),
                        ReadEvent::Line => bytestream::read_line(&mut guard).await,
                    };
                    Some(out)
                } => r,
            };
            let step = match step {
                Some(s) => s,
                None => break None,
            };
            match step {
                Ok(Some(bytes)) => match lua.create_string(&bytes) {
                    Ok(s) => spawn_handler(&lua, &sched2, what, &cb, s),
                    Err(e) => break Some(e.to_string()),
                },
                Ok(None) => break None,
                Err(e) => break Some(e.to_string()),
            }
        };
        fire_close(&lua, &sched2, close_what, &close_state, err);
    });
}

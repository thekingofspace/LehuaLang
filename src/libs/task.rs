use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::thread::ThreadStatus;
use mlua::{
    Function, Lua, MetaMethod, MultiValue, Thread, UserData, UserDataMethods, Value,
};
use tokio::sync::Notify;

use super::LibCtx;
use crate::engine::VmScheduler;
use crate::error::LehuaError;

struct TaskState {
    kind: &'static str,
    cancelled: Cell<bool>,
    finished: Cell<bool>,
    time: Cell<f64>,
    wake: Notify,
    thread: RefCell<Option<Thread>>,
}

impl TaskState {
    fn new(kind: &'static str, time: f64) -> Rc<Self> {
        Rc::new(TaskState {
            kind,
            cancelled: Cell::new(false),
            finished: Cell::new(false),
            time: Cell::new(time),
            wake: Notify::new(),
            thread: RefCell::new(None),
        })
    }

    async fn sleep(&self) -> bool {
        loop {
            if self.cancelled.get() {
                return false;
            }
            let dur = to_duration(self.time.get());
            tokio::select! {
                _ = self.wake.notified() => {
                    if self.cancelled.get() {
                        return false;
                    }
                }
                _ = tokio::time::sleep(dur) => return true,
            }
        }
    }

    async fn wait_cancelled(&self) {
        loop {
            if self.cancelled.get() {
                return;
            }
            let notified = self.wake.notified();
            if self.cancelled.get() {
                return;
            }
            notified.await;
        }
    }
}

type Registry = Rc<RefCell<std::collections::HashMap<usize, Rc<TaskState>>>>;

const MAX_TASK_SECONDS: f64 = 60.0 * 60.0 * 24.0 * 365.0 * 100.0;

fn to_duration(seconds: f64) -> Duration {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(seconds.min(MAX_TASK_SECONDS))
}

fn register_thread(registry: &Registry, thread: &Thread, state: &Rc<TaskState>) {
    registry
        .borrow_mut()
        .insert(thread.to_pointer() as usize, state.clone());
}

fn unregister_thread(registry: &Registry, key: usize, state: &Rc<TaskState>) {
    let mut reg = registry.borrow_mut();
    if reg.get(&key).map(|s| Rc::ptr_eq(s, state)).unwrap_or(false) {
        reg.remove(&key);
    }
}

fn reschedule_state(state: &Rc<TaskState>, seconds: f64) -> mlua::Result<()> {
    if state.kind != "delay" && state.kind != "schedule" {
        return Err(LehuaError::msg(format!(
            "a {} task has no timer to change",
            state.kind
        ))
        .into());
    }
    state.time.set(seconds);
    state.wake.notify_waiters();
    Ok(())
}

pub struct TaskHandle {
    state: Rc<TaskState>,
}

struct Guard {
    sched: Rc<VmScheduler>,
}

impl Guard {
    fn new(sched: &Rc<VmScheduler>) -> Self {
        sched.retain_task();
        Guard {
            sched: sched.clone(),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.sched.release_task();
    }
}

enum Runnable {
    Func(Function),
    Coroutine(Thread),
}

fn to_runnable(v: Value) -> mlua::Result<Runnable> {
    match v {
        Value::Function(f) => Ok(Runnable::Func(f)),
        Value::Thread(t) => Ok(Runnable::Coroutine(t)),
        other => Err(LehuaError::msg(format!(
            "expected a function or coroutine, got {}",
            other.type_name()
        ))
        .into()),
    }
}

fn close_thread(lua: &Lua, thread: &Thread) {
    let close = lua
        .globals()
        .get::<mlua::Table>("coroutine")
        .and_then(|c| c.get::<Function>("close"));
    if let Ok(close) = close {
        let _ = close.call::<()>(thread.clone());
    }
}

fn cancel_state(lua: &Lua, state: &Rc<TaskState>) {
    state.cancelled.set(true);
    state.wake.notify_waiters();
    let thread = state.thread.borrow().clone();
    if let Some(t) = thread {
        close_thread(lua, &t);
    }
}

async fn drive(
    lua: Lua,
    state: Rc<TaskState>,
    registry: Registry,
    thread: Thread,
    args: MultiValue,
) {
    if state.cancelled.get() {
        return;
    }
    *state.thread.borrow_mut() = Some(thread.clone());
    let key = thread.to_pointer() as usize;
    register_thread(&registry, &thread, &state);
    match thread.into_async::<()>(args) {
        Ok(fut) => {
            tokio::select! {
                _ = state.wait_cancelled() => {}
                r = fut => {
                    if let Err(e) = r {
                        if !state.cancelled.get() {
                            eprintln!("lehua: task error: {}", crate::error::pretty(&e));
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("lehua: task error: {e}"),
    }
    unregister_thread(&registry, key, &state);
    let _ = lua;
}

fn make_thread(lua: &Lua, runnable: &Runnable) -> mlua::Result<Thread> {
    match runnable {
        Runnable::Func(f) => lua.create_thread(f.clone()),
        Runnable::Coroutine(t) => Ok(t.clone()),
    }
}

fn spawn_one(
    lua: &Lua,
    sched: &Rc<VmScheduler>,
    registry: &Registry,
    state: Rc<TaskState>,
    runnable: Runnable,
    args: MultiValue,
    deferred: bool,
) -> mlua::Result<()> {
    let thread = make_thread(lua, &runnable)?;
    register_thread(registry, &thread, &state);
    let lua = lua.clone();
    let registry = registry.clone();
    let guard = Guard::new(sched);
    tokio::task::spawn_local(async move {
        let _guard = guard;
        if deferred {
            tokio::task::yield_now().await;
        }
        drive(lua, state.clone(), registry, thread, args).await;
        state.finished.set(true);
    });
    Ok(())
}

fn spawn_delayed(
    lua: &Lua,
    sched: &Rc<VmScheduler>,
    registry: &Registry,
    state: Rc<TaskState>,
    runnable: Runnable,
    args: MultiValue,
) {
    if let Runnable::Coroutine(t) = &runnable {
        *state.thread.borrow_mut() = Some(t.clone());
        register_thread(registry, t, &state);
    }
    let lua = lua.clone();
    let registry = registry.clone();
    let guard = Guard::new(sched);
    tokio::task::spawn_local(async move {
        let _guard = guard;
        if state.sleep().await {
            match make_thread(&lua, &runnable) {
                Ok(thread) => drive(lua, state.clone(), registry.clone(), thread, args).await,
                Err(e) => eprintln!("lehua: task error: {e}"),
            }
        }
        if let Runnable::Coroutine(t) = &runnable {
            unregister_thread(&registry, t.to_pointer() as usize, &state);
        }
        state.finished.set(true);
    });
}

fn spawn_scheduled(
    lua: &Lua,
    sched: &Rc<VmScheduler>,
    registry: &Registry,
    state: Rc<TaskState>,
    runnable: Runnable,
    args: MultiValue,
) {
    if let Runnable::Coroutine(t) = &runnable {
        *state.thread.borrow_mut() = Some(t.clone());
        register_thread(registry, t, &state);
    }
    let lua = lua.clone();
    let sched = sched.clone();
    let registry = registry.clone();
    let guard = Guard::new(&sched);
    tokio::task::spawn_local(async move {
        let _guard = guard;
        loop {
            if !state.sleep().await {
                break;
            }
            match &runnable {
                Runnable::Func(f) => {
                    let run_state = state.clone();
                    match lua.create_thread(f.clone()) {
                        Ok(thread) => {
                            *state.thread.borrow_mut() = Some(thread.clone());
                            let lua2 = lua.clone();
                            let args2 = args.clone();
                            let registry2 = registry.clone();
                            let inner = Guard::new(&sched);
                            tokio::task::spawn_local(async move {
                                let _inner = inner;
                                drive(lua2, run_state, registry2, thread, args2).await;
                            });
                        }
                        Err(e) => eprintln!("lehua: task error: {e}"),
                    }
                }
                Runnable::Coroutine(t) => {
                    if t.status() != ThreadStatus::Resumable {
                        break;
                    }
                    drive(
                        lua.clone(),
                        state.clone(),
                        registry.clone(),
                        t.clone(),
                        args.clone(),
                    )
                    .await;
                }
            }
        }
        if let Runnable::Coroutine(t) = &runnable {
            unregister_thread(&registry, t.to_pointer() as usize, &state);
        }
        state.finished.set(true);
    });
}

fn handle_from(v: &Value) -> Option<Rc<TaskState>> {
    v.as_userdata()
        .and_then(|u| u.borrow::<TaskHandle>().ok())
        .map(|h| h.state.clone())
}

impl UserData for TaskHandle {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Cancel", |lua, this, ()| {
            cancel_state(lua, &this.state);
            Ok(())
        });

        m.add_method("Reschedule", |_, this, seconds: f64| {
            reschedule_state(&this.state, seconds)
        });

        m.add_method("IsCancelled", |_, this, ()| Ok(this.state.cancelled.get()));
        m.add_method("IsFinished", |_, this, ()| Ok(this.state.finished.get()));
        m.add_method("Kind", |_, this, ()| Ok(this.state.kind));

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Task({})", this.state.kind))
        });
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;
    let registry: Registry = Rc::new(RefCell::new(std::collections::HashMap::new()));

    t.set(
        "wait",
        lua.create_async_function(|_, seconds: Option<f64>| async move {
            let start = Instant::now();
            tokio::time::sleep(to_duration(seconds.unwrap_or(0.0))).await;
            Ok(start.elapsed().as_secs_f64())
        })?,
    )?;

    {
        let sched = ctx.sched.clone();
        let registry = registry.clone();
        t.set(
            "spawn",
            lua.create_function(move |lua, mut args: MultiValue| {
                let target = args.pop_front().unwrap_or(Value::Nil);
                let runnable = to_runnable(target)?;
                let state = TaskState::new("spawn", 0.0);
                spawn_one(lua, &sched, &registry, state.clone(), runnable, args, false)?;
                Ok(TaskHandle { state })
            })?,
        )?;
    }

    {
        let sched = ctx.sched.clone();
        let registry = registry.clone();
        t.set(
            "defer",
            lua.create_function(move |lua, mut args: MultiValue| {
                let target = args.pop_front().unwrap_or(Value::Nil);
                let runnable = to_runnable(target)?;
                let state = TaskState::new("defer", 0.0);
                spawn_one(lua, &sched, &registry, state.clone(), runnable, args, true)?;
                Ok(TaskHandle { state })
            })?,
        )?;
    }

    {
        let sched = ctx.sched.clone();
        let registry = registry.clone();
        t.set(
            "delay",
            lua.create_function(move |lua, mut args: MultiValue| {
                let seconds = match args.pop_front() {
                    Some(Value::Number(n)) => n,
                    Some(Value::Integer(i)) => i as f64,
                    _ => {
                        return Err(LehuaError::msg(
                            "task.delay(seconds, callback, ...) expects a number first",
                        )
                        .into())
                    }
                };
                let target = args.pop_front().unwrap_or(Value::Nil);
                let runnable = to_runnable(target)?;
                let state = TaskState::new("delay", seconds);
                spawn_delayed(lua, &sched, &registry, state.clone(), runnable, args);
                Ok(TaskHandle { state })
            })?,
        )?;
    }

    {
        let sched = ctx.sched.clone();
        let registry = registry.clone();
        t.set(
            "schedule",
            lua.create_function(move |lua, mut args: MultiValue| {
                let target = args.pop_front().unwrap_or(Value::Nil);
                let runnable = to_runnable(target)?;
                let seconds = match args.pop_front() {
                    Some(Value::Number(n)) => n,
                    Some(Value::Integer(i)) => i as f64,
                    _ => {
                        return Err(LehuaError::msg(
                            "task.schedule(callback, seconds, ...) expects a number second",
                        )
                        .into())
                    }
                };
                let state = TaskState::new("schedule", seconds);
                spawn_scheduled(lua, &sched, &registry, state.clone(), runnable, args);
                Ok(TaskHandle { state })
            })?,
        )?;
    }

    t.set(
        "reschedule",
        lua.create_function(|_, (task, seconds): (Value, f64)| {
            let state = handle_from(&task)
                .ok_or_else(|| LehuaError::msg("task.reschedule expects a task object"))?;
            reschedule_state(&state, seconds)
        })?,
    )?;

    {
        let registry = registry.clone();
        t.set(
            "cancel",
            lua.create_function(move |lua, target: Value| {
                if let Some(state) = handle_from(&target) {
                    cancel_state(lua, &state);
                    return Ok(());
                }
                match target {
                    Value::Thread(t) => {
                        let key = t.to_pointer() as usize;
                        let state = registry.borrow().get(&key).cloned();
                        match state {
                            Some(state) => cancel_state(lua, &state),
                            None => close_thread(lua, &t),
                        }
                        Ok(())
                    }
                    other => Err(LehuaError::msg(format!(
                        "task.cancel expects a task or coroutine, got {}",
                        other.type_name()
                    ))
                    .into()),
                }
            })?,
        )?;
    }

    Ok(Value::Table(t))
}

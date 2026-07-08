--!strict
-- Lehua ambient type definitions.
-- Files ending in `.d.luau` are for the type checker only and are skipped
-- during compilation.

export type Port = {
    Push: (self: Port, value: any) -> boolean,
    Pop: (self: Port) -> any,
    TryPop: (self: Port) -> any,
    Close: (self: Port) -> (),
    IsClosed: (self: Port) -> boolean,
}

export type FileHandle = {
    read: (self: FileHandle, fmt: (number | string)?) -> (string | number)?,
    write: (self: FileHandle, ...string | number) -> (),
    seek: (self: FileHandle, whence: string?, offset: number?) -> number,
    lines: (self: FileHandle) -> () -> string?,
    flush: (self: FileHandle) -> (),
    close: (self: FileHandle) -> (),
}

-- All fs path arguments accept `./`, `../`, absolute paths, `@self/...`
-- (this module's folder) and `@Alias/...` aliases from .luaurc.
export type FS = {
    dir: string,
    resolve: (path: string) -> string,
    join: (...string) -> string,
    dirname: (path: string) -> string,
    basename: (path: string) -> string,
    extname: (path: string) -> string,
    cwd: () -> string,
    exists: (path: string) -> boolean,
    isFile: (path: string) -> boolean,
    isDir: (path: string) -> boolean,
    read: (path: string) -> string,
    write: (path: string, data: string) -> (),
    append: (path: string, data: string) -> (),
    mkdir: (path: string) -> (),
    remove: (path: string) -> (),
    removeAll: (path: string) -> (),
    readDir: (path: string) -> { string },
    open: (path: string, mode: string?) -> FileHandle,
}

export type RunResult = {
    ok: boolean,
    code: number,
    stdout: string,
    stderr: string,
}

export type Process = {
    args: { string },
    pid: number,
    platform: string,
    arch: string,
    exepath: () -> string,
    cwd: () -> string,
    chdir: (path: string) -> (),
    exit: (code: number?) -> never,
    getenv: (name: string) -> string?,
    setenv: (name: string, value: string?) -> (),
    env: () -> { [string]: string },
    run: (program: string, args: { string }?, opts: { cwd: string? }?) -> RunResult,
    exec: (commandline: string, opts: { cwd: string? }?) -> RunResult,
    sleep: (ms: number) -> (),
    memory: () -> { total: number, available: number, used: number, processUsed: number? },
    cpus: () -> number,
    system: () -> { os: string, arch: string, family: string, name: string, kernel: string, version: string, hostname: string },
    -- Runs `callback(dt)` about 60 times a second after your main code returns.
    -- Binding the same name again replaces the callback.
    BindToHeartbeat: (name: string, callback: (dt: number) -> ()) -> (),
    -- Removes the heartbeat binding with that name. Returns true if one existed.
    UnbindFromHeartbeat: (name: string) -> boolean,
    -- Runs `callback(code)` when the program is about to end.
    BindToClose: (callback: (code: number) -> ()) -> (),
}

-- Spawn `path` on its own thread + VM. Returns the parent end of the channel.
declare function parallel(path: string, ...: any): Port

-- Provided per-module by the runtime:
declare __dirname: string    -- absolute directory of the current module
declare __filename: string   -- absolute path of the current module
declare channel: Port        -- inside a worker: this worker's channel endpoint
declare fs: FS               -- available in modules with --#include[fs]
declare process: Process     -- available in modules with --#include[process]

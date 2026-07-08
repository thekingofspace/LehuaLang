--!strict

export type Port = {
    Push: (self: Port, value: any) -> boolean,
    Pop: (self: Port) -> any,
    TryPop: (self: Port) -> any,
    Close: (self: Port) -> (),
    IsClosed: (self: Port) -> boolean,
}

export type Metadata = {
    type: "file" | "dir" | "other",
    size: number,
    readonly: boolean,
    modifiedAt: number?,
    createdAt: number?,
    accessedAt: number?,
}

export type FileHandle = {
    read: (self: FileHandle, fmt: (number | string)?) -> (string | number)?,
    write: (self: FileHandle, ...string | number) -> (),
    seek: (self: FileHandle, whence: ("set" | "cur" | "end")?, offset: number?) -> number,
    lines: (self: FileHandle) -> () -> string?,
    flush: (self: FileHandle) -> (),
    close: (self: FileHandle) -> (),
}

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
    metadata: (path: string) -> Metadata,
    read: (path: string) -> string,
    write: (path: string, data: string) -> (),
    readBuffer: (path: string) -> buffer,
    writeBuffer: (path: string, data: buffer) -> (),
    append: (path: string, data: string) -> (),
    copy: (source: string, destination: string) -> (),
    copyAll: (source: string, destination: string) -> (),
    move: (source: string, destination: string) -> (),
    glob: (pattern: string) -> { string },
    tempDir: () -> string,
    tempFile: (extension: string?) -> string,
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
    BindToHeartbeat: (name: string, callback: (dt: number) -> ()) -> (),
    UnbindFromHeartbeat: (name: string) -> boolean,
    BindToClose: (callback: (code: number) -> ()) -> (),
}

export type SerdeFormat = "json" | "yaml" | "yml" | "toml"

export type Serde = {
    formats: { SerdeFormat },
    null: any,
    array: (t: { any }?) -> { any },
    encode: (format: SerdeFormat, data: any, pretty: boolean?) -> string,
    decode: (format: SerdeFormat, text: string) -> any,
}

export type HashAlgorithm = "md5" | "sha1" | "sha224" | "sha256" | "sha384" | "sha512"
export type HmacAlgorithm = "md5" | "sha1" | "sha256" | "sha384" | "sha512"
export type CompressionFormat = "gzip" | "gz" | "zlib" | "deflate"

export type Cryptography = {
    hash: (algorithm: HashAlgorithm, data: string) -> string,
    hmac: (algorithm: HmacAlgorithm, key: string, data: string) -> string,
    crc32: (data: string) -> number,
    compress: (data: string, format: CompressionFormat?, level: number?) -> string,
    decompress: (data: string, format: CompressionFormat?) -> string,
    base64Encode: (data: string) -> string,
    base64Decode: (text: string) -> string,
    hexEncode: (data: string) -> string,
    hexDecode: (text: string) -> string,
    encrypt: (key: string, plaintext: string) -> string,
    decrypt: (key: string, data: string) -> string,
    passwordHash: (password: string) -> string,
    passwordVerify: (password: string, hash: string) -> boolean,
    randomBytes: (count: number) -> string,
    uuid: () -> string,
}

export type DateTimeComponents = {
    year: number,
    month: number,
    day: number,
    hour: number,
    minute: number,
    second: number,
    millisecond: number,
    weekday: number,
    yearday: number,
    offset: number,
}

export type DateTimeDelta = {
    years: number?,
    months: number?,
    days: number?,
    hours: number?,
    minutes: number?,
    seconds: number?,
    milliseconds: number?,
}

export type DateTime = {
    toUnix: (self: DateTime) -> number,
    toMillis: (self: DateTime) -> number,
    toISO: (self: DateTime) -> string,
    format: (self: DateTime, format: string?, utc: boolean?) -> string,
    components: (self: DateTime, utc: boolean?) -> DateTimeComponents,
    add: (self: DateTime, delta: DateTimeDelta, utc: boolean?) -> DateTime,
    offset: (self: DateTime) -> number,
}

export type DateTimeLib = {
    now: () -> DateTime,
    fromUnix: (seconds: number) -> DateTime,
    fromMillis: (ms: number) -> DateTime,
    fromISO: (text: string) -> DateTime,
    parse: (text: string, format: string, utc: boolean?) -> DateTime,
    fromComponents: (c: {
        year: number,
        month: number?,
        day: number?,
        hour: number?,
        minute: number?,
        second: number?,
        millisecond: number?,
    }, utc: boolean?) -> DateTime,
    isDateTime: (value: any) -> boolean,
}

export type RegexMatch = {
    text: string,
    start: number,
    finish: number,
}

export type RegexCaptures = {
    match: RegexMatch,
    [number]: RegexMatch | boolean,
    [string]: RegexMatch,
}

export type Regex = {
    isMatch: (self: Regex, text: string) -> boolean,
    find: (self: Regex, text: string, init: number?) -> RegexMatch?,
    findAll: (self: Regex, text: string) -> { RegexMatch },
    captures: (self: Regex, text: string) -> RegexCaptures?,
    capturesAll: (self: Regex, text: string) -> { RegexCaptures },
    replace: (self: Regex, text: string, replacement: string, limit: number?) -> string,
    replaceAll: (self: Regex, text: string, replacement: string) -> string,
    split: (self: Regex, text: string, limit: number?) -> { string },
}

export type RegexLib = {
    new: (pattern: string, flags: string?) -> Regex,
    escape: (text: string) -> string,
    isMatch: (pattern: string, text: string) -> boolean,
}

export type StdioColor =
    "reset" | "black" | "red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "white"
    | "brightBlack" | "brightRed" | "brightGreen" | "brightYellow" | "brightBlue"
    | "brightMagenta" | "brightCyan" | "brightWhite"

export type StdioStyle =
    "reset" | "bold" | "dim" | "italic" | "underline" | "blink" | "reverse" | "strikethrough"

export type StdioStream = "stdin" | "stdout" | "stderr"

export type Stdio = {
    write: (...(string | number | boolean)) -> (),
    ewrite: (...(string | number | boolean)) -> (),
    readLine: () -> string?,
    readAll: () -> string,
    prompt: (message: string?, options: { string }?) -> (string?, number?),
    isTTY: (stream: StdioStream?) -> boolean,
    color: (name: StdioColor) -> string,
    bgColor: (name: StdioColor) -> string,
    style: (name: StdioStyle) -> string,
    stripColors: (text: string) -> string,
}

export type LuauLoadOptions = {
    name: string?,
    environment: { [string]: any }?,
}

export type LuauCompileOptions = {
    optimizationLevel: number?,
    debugLevel: number?,
    coverageLevel: number?,
}

export type Luau = {
    load: (source: string, options: LuauLoadOptions?) -> (...any) -> ...any,
    eval: (source: string, options: LuauLoadOptions?) -> ...any,
    compile: (source: string, options: LuauCompileOptions?) -> string,
}

export type UrlParts = {
    href: string,
    scheme: string,
    host: string?,
    port: number?,
    path: string,
    username: string?,
    password: string?,
    query: string?,
    queryParams: { [string]: string }?,
    fragment: string?,
}

export type Url = {
    parse: (text: string) -> UrlParts,
    join: (base: string, reference: string) -> string,
    isValid: (text: string) -> boolean,
    encode: (text: string) -> string,
    decode: (text: string) -> string,
    encodeQuery: (params: { [string]: string | number | boolean }) -> string,
    decodeQuery: (text: string) -> { [string]: string },
}

export type SemverParts = {
    major: number,
    minor: number,
    patch: number,
    prerelease: string?,
    build: string?,
}

export type Semver = {
    parse: (version: string) -> SemverParts,
    isValid: (version: string) -> boolean,
    compare: (a: string, b: string) -> number,
    satisfies: (version: string, requirement: string) -> boolean,
    sort: (versions: { string }) -> { string },
}

export type ArchiveEntry = {
    name: string,
    size: number,
    compressedSize: number?,
    isDir: boolean,
}

export type Archive = {
    zip: (destination: string, sources: string | { string }, options: { level: number? }?) -> (),
    unzip: (source: string, destination: string) -> (),
    listZip: (source: string) -> { ArchiveEntry },
    tar: (destination: string, sources: string | { string }) -> (),
    untar: (source: string, destination: string) -> (),
    listTar: (source: string) -> { ArchiveEntry },
}

export type SqliteParams = { any } | { [string]: any }

export type SqliteRow = { [string]: any }

export type SqliteDatabase = {
    execute: (self: SqliteDatabase, sql: string, params: SqliteParams?) -> number,
    executeBatch: (self: SqliteDatabase, sql: string) -> (),
    query: (self: SqliteDatabase, sql: string, params: SqliteParams?) -> { SqliteRow },
    queryOne: (self: SqliteDatabase, sql: string, params: SqliteParams?) -> SqliteRow?,
    transaction: (self: SqliteDatabase, callback: () -> ...any) -> ...any,
    lastInsertId: (self: SqliteDatabase) -> number,
    changes: (self: SqliteDatabase) -> number,
    tables: (self: SqliteDatabase) -> { string },
    isOpen: (self: SqliteDatabase) -> boolean,
    close: (self: SqliteDatabase) -> (),
}

export type Sqlite = {
    fromLocal: (path: string) -> SqliteDatabase,
    fromConnection: (connection: string) -> SqliteDatabase,
    memory: () -> SqliteDatabase,
}

export type MongoObjectId = {
    hex: (self: MongoObjectId) -> string,
}

export type MongoDocument = { [string]: any }

export type MongoSortKeys = { [string]: number } | { { any } }

export type MongoUpdateResult = {
    matched: number,
    modified: number,
    upsertedId: any,
}

export type MongoFindOptions = {
    sort: MongoSortKeys?,
    projection: MongoDocument?,
    limit: number?,
    skip: number?,
    page: number?,
    pageSize: number?,
}

export type MongoOperator = { [string]: any }

export type MongoCollection = {
    name: (self: MongoCollection) -> string,
    insertOne: (self: MongoCollection, document: MongoDocument) -> any,
    insertMany: (self: MongoCollection, documents: { MongoDocument }) -> { any },
    findOne: (self: MongoCollection, filter: MongoDocument?, options: MongoFindOptions?) -> MongoDocument?,
    find: (self: MongoCollection, filter: MongoDocument?, options: MongoFindOptions?) -> { MongoDocument },
    updateOne: (self: MongoCollection, filter: MongoDocument, update: MongoDocument, options: { upsert: boolean? }?) -> MongoUpdateResult,
    updateMany: (self: MongoCollection, filter: MongoDocument, update: MongoDocument, options: { upsert: boolean? }?) -> MongoUpdateResult,
    replaceOne: (self: MongoCollection, filter: MongoDocument, replacement: MongoDocument, options: { upsert: boolean? }?) -> MongoUpdateResult,
    deleteOne: (self: MongoCollection, filter: MongoDocument) -> number,
    deleteMany: (self: MongoCollection, filter: MongoDocument) -> number,
    count: (self: MongoCollection, filter: MongoDocument?) -> number,
    distinct: (self: MongoCollection, field: string, filter: MongoDocument?) -> { any },
    aggregate: (self: MongoCollection, pipeline: { MongoDocument }) -> { MongoDocument },
    createIndex: (self: MongoCollection, keys: MongoSortKeys, options: { unique: boolean?, name: string?, sparse: boolean? }?) -> string,
    indexes: (self: MongoCollection) -> { string },
    dropIndex: (self: MongoCollection, name: string) -> (),
    drop: (self: MongoCollection) -> (),
}

export type MongoDatabase = {
    name: (self: MongoDatabase) -> string,
    collection: (self: MongoDatabase, name: string) -> MongoCollection,
    collections: (self: MongoDatabase) -> { string },
    createCollection: (self: MongoDatabase, name: string) -> (),
    runCommand: (self: MongoDatabase, command: MongoDocument) -> MongoDocument,
    drop: (self: MongoDatabase) -> (),
}

export type MongoCluster = {
    database: (self: MongoCluster, name: string) -> MongoDatabase,
    defaultDatabase: (self: MongoCluster) -> MongoDatabase?,
    databases: (self: MongoCluster) -> { string },
    close: (self: MongoCluster) -> (),
}

export type MongoConnectOptions = {
    serverSelectionTimeoutMs: number?,
    connectTimeoutMs: number?,
    appName: string?,
}

export type Mongo = {
    connect: (connectionString: string, options: MongoConnectOptions?) -> MongoCluster,
    objectId: (hex: string?) -> MongoObjectId,
    isObjectId: (value: any) -> boolean,
    null: any,
    eq: (value: any) -> MongoOperator,
    ne: (value: any) -> MongoOperator,
    gt: (value: any) -> MongoOperator,
    gte: (value: any) -> MongoOperator,
    lt: (value: any) -> MongoOperator,
    lte: (value: any) -> MongoOperator,
    between: (min: any, max: any) -> MongoOperator,
    inList: (values: { any }) -> MongoOperator,
    notInList: (values: { any }) -> MongoOperator,
    allIn: (values: { any }) -> MongoOperator,
    size: (count: number) -> MongoOperator,
    exists: (flag: boolean?) -> MongoOperator,
    regex: (pattern: string, flags: string?) -> MongoOperator,
    notOf: (operator: MongoOperator) -> MongoOperator,
    anyOf: (filters: { MongoDocument }) -> MongoDocument,
    allOf: (filters: { MongoDocument }) -> MongoDocument,
    noneOf: (filters: { MongoDocument }) -> MongoDocument,
}

declare function parallel(path: string, ...: any): Port

declare __dirname: string
declare __filename: string
declare channel: Port
declare fs: FS
declare process: Process
declare serde: Serde
declare cryptography: Cryptography
declare datetime: DateTimeLib
declare regex: RegexLib
declare stdio: Stdio
declare luau: Luau
declare url: Url
declare semver: Semver
declare archive: Archive
declare sqlite: Sqlite
declare mongo: Mongo

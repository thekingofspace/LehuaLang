--!strict

export type Port = {
    Push: (self: Port, value: any) -> boolean,
    Pop: (self: Port) -> any,
    TryPop: (self: Port) -> any,
    Close: (self: Port) -> (),
    IsClosed: (self: Port) -> boolean,
}

export type Subscription = {
    Unsubscribe: (self: Subscription) -> boolean,
    Topic: (self: Subscription) -> string,
    IsActive: (self: Subscription) -> boolean,
}

export type Messenger = {
    Publish: (self: Messenger, topic: string, ...any) -> number,
    Subscribe: (self: Messenger, topic: string, callback: (...any) -> ()) -> Subscription,
    Once: (self: Messenger, topic: string, callback: (...any) -> ()) -> Subscription,
    Subscribers: (self: Messenger, topic: string) -> number,
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
    readBufferAt: (path: string, start: number, endpoint: number) -> buffer,
    writeBufferAt: (path: string, start: number, data: buffer) -> (),
    readImage: (path: string) -> Canvas,
    writeImage: (path: string, image: Canvas, format: ImageFormat?, quality: number?) -> string,
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
    loadEnv: (path: string?, options: { apply: boolean?, override: boolean? }?) -> { [string]: string },
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
    [any]: RegexMatch | boolean,
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
    writeLine: (...(string | number | boolean)) -> (),
    ewriteLine: (...(string | number | boolean)) -> (),
    readLine: () -> string?,
    readAll: () -> string,
    prompt: (message: string?, options: { string }?) -> (string?, number?),
    isTTY: (stream: StdioStream?) -> boolean,
    color: (name: StdioColor | string) -> string,
    bgColor: (name: StdioColor | string) -> string,
    rgb: (r: number, g: number, b: number) -> string,
    bgRgb: (r: number, g: number, b: number) -> string,
    style: (name: StdioStyle) -> string,
    stripColors: (text: string) -> string,
    clear: () -> (),
    clearLine: () -> (),
    transcribe: (fn: () -> ()) -> string,
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

export type SqliteJournalMode = "delete" | "truncate" | "persist" | "memory" | "wal" | "off"

export type SqliteOpenOptions = {
    readOnly: boolean?,
    create: boolean?,
    password: string?,
    busyTimeoutMs: number?,
    foreignKeys: boolean?,
    journalMode: SqliteJournalMode?,
    cacheSizeKb: number?,
    pragmas: { [string]: string | number | boolean }?,
}

export type SqliteConnectionInfo = {
    file: string?,
    readOnly: boolean?,
    create: boolean?,
    cache: ("shared" | "private")?,
    immutable: boolean?,
    password: string?,
    busyTimeoutMs: number?,
    foreignKeys: boolean?,
    journalMode: SqliteJournalMode?,
    cacheSizeKb: number?,
    pragmas: { [string]: string | number | boolean }?,
}

export type SqliteInfo = {
    path: string,
    readOnly: boolean,
    autocommit: boolean,
    totalChanges: number,
    lastInsertId: number,
    sqliteVersion: string,
    journalMode: string,
    encoding: string,
    foreignKeys: boolean,
    busyTimeoutMs: number,
    pageSize: number,
    pageCount: number,
    cacheSize: number,
    userVersion: number,
    schemaVersion: number,
}

export type SqliteDatabase = {
    execute: (self: SqliteDatabase, sql: string, params: SqliteParams?) -> number,
    executeBatch: (self: SqliteDatabase, sql: string) -> (),
    query: (self: SqliteDatabase, sql: string, params: SqliteParams?) -> { SqliteRow },
    queryOne: (self: SqliteDatabase, sql: string, params: SqliteParams?) -> SqliteRow?,
    transaction: (self: SqliteDatabase, callback: () -> ...any) -> ...any,
    lastInsertId: (self: SqliteDatabase) -> number,
    changes: (self: SqliteDatabase) -> number,
    tables: (self: SqliteDatabase) -> { string },
    info: (self: SqliteDatabase) -> SqliteInfo,
    isOpen: (self: SqliteDatabase) -> boolean,
    close: (self: SqliteDatabase) -> (),
}

export type Sqlite = {
    fromLocal: (path: string, options: SqliteOpenOptions?) -> SqliteDatabase,
    fromConnection: (connection: string | SqliteConnectionInfo, options: SqliteOpenOptions?) -> SqliteDatabase,
    memory: (options: SqliteOpenOptions?) -> SqliteDatabase,
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

export type MongoUpdate = MongoDocument | { MongoDocument }

export type MongoCollection = {
    name: (self: MongoCollection) -> string,
    insertOne: (self: MongoCollection, document: MongoDocument) -> any,
    insertMany: (self: MongoCollection, documents: { MongoDocument }) -> { any },
    findOne: (self: MongoCollection, filter: MongoDocument?, options: MongoFindOptions?) -> MongoDocument?,
    find: (self: MongoCollection, filter: MongoDocument?, options: MongoFindOptions?) -> { MongoDocument },
    updateOne: (self: MongoCollection, filter: MongoDocument, update: MongoUpdate, options: { upsert: boolean? }?) -> MongoUpdateResult,
    updateMany: (self: MongoCollection, filter: MongoDocument, update: MongoUpdate, options: { upsert: boolean? }?) -> MongoUpdateResult,
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
    set: (fields: MongoDocument) -> MongoOperator,
    setOnInsert: (fields: MongoDocument) -> MongoOperator,
    unset: (fields: string | { string }) -> MongoOperator,
    increment: (fields: { [string]: number }) -> MongoOperator,
    multiply: (fields: { [string]: number }) -> MongoOperator,
    rename: (fields: { [string]: string }) -> MongoOperator,
    setMin: (fields: MongoDocument) -> MongoOperator,
    setMax: (fields: MongoDocument) -> MongoOperator,
    setNow: (fields: string | { string }) -> MongoOperator,
    push: (fields: MongoDocument) -> MongoOperator,
    pushAll: (fields: { [string]: { any } }) -> MongoOperator,
    pull: (fields: MongoDocument) -> MongoOperator,
    pullAll: (fields: { [string]: { any } }) -> MongoOperator,
    addToSet: (fields: MongoDocument) -> MongoOperator,
    popFirst: (fields: string | { string }) -> MongoOperator,
    popLast: (fields: string | { string }) -> MongoOperator,
}

export type Sink = {
    write: (self: Sink, data: string | buffer) -> (),
    bytes: (self: Sink) -> string,
    buffer: (self: Sink) -> buffer,
    len: (self: Sink) -> number,
    clear: (self: Sink) -> (),
    save: (self: Sink, path: string) -> string,
    isFile: (self: Sink) -> boolean,
    path: (self: Sink) -> string?,
}

export type TcpConnection = {
    on: ((self: TcpConnection, event: "data", callback: (data: string) -> ()) -> ())
        & ((self: TcpConnection, event: "line", callback: (line: string) -> ()) -> ())
        & ((self: TcpConnection, event: "close", callback: (err: string?) -> ()) -> ()),
    read: (self: TcpConnection, count: number?) -> string?,
    readLine: (self: TcpConnection) -> string?,
    readUntil: (self: TcpConnection, delimiter: string) -> string?,
    readAll: (self: TcpConnection) -> string,
    write: (self: TcpConnection, data: string | buffer) -> (),
    peerAddress: (self: TcpConnection) -> string,
    localAddress: (self: TcpConnection) -> string,
    close: (self: TcpConnection) -> (),
}

export type TcpServer = {
    on: (self: TcpServer, event: "connection", callback: (connection: TcpConnection) -> ()) -> (),
    accept: (self: TcpServer) -> TcpConnection?,
    address: (self: TcpServer) -> string,
    close: (self: TcpServer) -> (),
}

export type UdpSocket = {
    on: (self: UdpSocket, event: "message", callback: (data: string, ip: string, port: number) -> ()) -> (),
    sendTo: (self: UdpSocket, data: string | buffer, host: string, port: number) -> number,
    recvFrom: (self: UdpSocket) -> (string, string, number),
    connect: (self: UdpSocket, host: string, port: number) -> (),
    send: (self: UdpSocket, data: string | buffer) -> number,
    recv: (self: UdpSocket) -> string,
    broadcast: (self: UdpSocket, on: boolean?) -> (),
    localAddress: (self: UdpSocket) -> string,
    close: (self: UdpSocket) -> (),
}

export type IpcConnection = {
    on: ((self: IpcConnection, event: "data", callback: (data: string) -> ()) -> ())
        & ((self: IpcConnection, event: "line", callback: (line: string) -> ()) -> ())
        & ((self: IpcConnection, event: "close", callback: (err: string?) -> ()) -> ()),
    read: (self: IpcConnection, count: number?) -> string?,
    readLine: (self: IpcConnection) -> string?,
    write: (self: IpcConnection, data: string | buffer) -> (),
    close: (self: IpcConnection) -> (),
}

export type IpcServer = {
    on: (self: IpcServer, event: "connection", callback: (connection: IpcConnection) -> ()) -> (),
    accept: (self: IpcServer) -> IpcConnection?,
    name: (self: IpcServer) -> string,
    close: (self: IpcServer) -> (),
}

export type HttpHeaders = { [string]: string }

export type HttpResponse = {
    ok: boolean,
    status: number,
    statusText: string,
    headers: HttpHeaders,
    body: string,
    url: string,
}

export type HttpRequestOptions = {
    method: string?,
    url: string?,
    headers: HttpHeaders?,
    query: { [string]: string }?,
    form: { [string]: string }?,
    body: (string | buffer)?,
    json: any?,
    timeout: number?,
    auth: { username: string?, password: string?, bearer: string? }?,
    redirects: (number | boolean)?,
    insecure: boolean?,
}

export type HttpServerResponse = {
    status: number?,
    headers: HttpHeaders?,
    body: (string | buffer)?,
    json: any?,
}

export type HttpIncoming = {
    method: (self: HttpIncoming) -> string,
    path: (self: HttpIncoming) -> string,
    target: (self: HttpIncoming) -> string,
    query: (self: HttpIncoming) -> string,
    peerAddress: (self: HttpIncoming) -> string,
    body: (self: HttpIncoming) -> string,
    headers: (self: HttpIncoming) -> HttpHeaders,
    header: (self: HttpIncoming, name: string) -> string?,
    respond: (self: HttpIncoming, response: string | HttpServerResponse) -> (),
}

export type HttpServer = {
    on: (self: HttpServer, event: "request", callback: (request: HttpIncoming) -> (string | HttpServerResponse)?) -> (),
    accept: (self: HttpServer) -> HttpIncoming?,
    address: (self: HttpServer) -> string,
    close: (self: HttpServer) -> (),
}

export type Http = {
    request: (url: string | HttpRequestOptions, options: HttpRequestOptions?) -> HttpResponse,
    get: (url: string, options: HttpRequestOptions?) -> HttpResponse,
    post: (url: string, options: HttpRequestOptions?) -> HttpResponse,
    put: (url: string, options: HttpRequestOptions?) -> HttpResponse,
    patch: (url: string, options: HttpRequestOptions?) -> HttpResponse,
    delete: (url: string, options: HttpRequestOptions?) -> HttpResponse,
    head: (url: string, options: HttpRequestOptions?) -> HttpResponse,
    options: (url: string, options: HttpRequestOptions?) -> HttpResponse,
    serve: (options: number | { host: string?, port: number }) -> HttpServer,
}

export type WebSocketMessage = {
    type: "text" | "binary" | "ping" | "pong" | "close" | "frame",
    data: string?,
    code: number?,
    reason: string?,
}

export type WebSocket = {
    on: ((self: WebSocket, event: "message", callback: (message: WebSocketMessage) -> ()) -> ())
        & ((self: WebSocket, event: "close", callback: (code: number?, reason: string?) -> ()) -> ()),
    send: (self: WebSocket, text: string) -> (),
    sendBinary: (self: WebSocket, data: string | buffer) -> (),
    ping: (self: WebSocket, data: string?) -> (),
    receive: (self: WebSocket) -> WebSocketMessage?,
    close: (self: WebSocket, code: number?, reason: string?) -> (),
    peerAddress: (self: WebSocket) -> string,
}

export type WebSocketServer = {
    on: (self: WebSocketServer, event: "connection", callback: (socket: WebSocket) -> ()) -> (),
    accept: (self: WebSocketServer) -> WebSocket?,
    address: (self: WebSocketServer) -> string,
    close: (self: WebSocketServer) -> (),
}

export type DnsMxRecord = { priority: number, host: string }
export type DnsSrvRecord = { priority: number, weight: number, port: number, target: string }

export type Dns = {
    lookup: (host: string) -> { string },
    lookupOne: (host: string) -> string?,
    reverse: (ip: string) -> { string },
    resolve: (name: string, recordType: string) -> { any },
}

export type FtpConnectOptions = {
    host: string,
    port: number?,
    user: string?,
    password: string?,
}

export type FtpSession = {
    pwd: (self: FtpSession) -> string,
    cwd: (self: FtpSession, path: string) -> (),
    cdup: (self: FtpSession) -> (),
    mkdir: (self: FtpSession, path: string) -> (),
    removeDir: (self: FtpSession, path: string) -> (),
    remove: (self: FtpSession, path: string) -> (),
    rename: (self: FtpSession, from: string, to: string) -> (),
    size: (self: FtpSession, path: string) -> number,
    list: (self: FtpSession, path: string?) -> { string },
    nameList: (self: FtpSession, path: string?) -> { string },
    binary: (self: FtpSession) -> (),
    ascii: (self: FtpSession) -> (),
    download: (self: FtpSession, remote: string, into: Sink?) -> string | number,
    downloadTo: (self: FtpSession, remote: string, localPath: string) -> string,
    upload: (self: FtpSession, remote: string, source: string | buffer | Sink) -> number,
    uploadFile: (self: FtpSession, remote: string, localPath: string) -> number,
    noop: (self: FtpSession) -> (),
    close: (self: FtpSession) -> (),
}

export type Ftp = {
    connect: (options: FtpConnectOptions) -> FtpSession,
}

export type WebSocketLib = {
    connect: (url: string) -> WebSocket,
    listen: (host: string?, port: number) -> WebSocketServer,
}

export type Net = {
    http: Http,
    ws: WebSocketLib,
    dns: Dns,
    ftp: Ftp,
    tcp: {
        connect: (host: string, port: number) -> TcpConnection,
        listen: (host: string?, port: number) -> TcpServer,
    },
    udp: {
        bind: (host: string?, port: number?) -> UdpSocket,
    },
    ipc: {
        connect: (name: string) -> IpcConnection,
        listen: (name: string) -> IpcServer,
    },
    sink: (options: { file: string }?) -> Sink,
}

export type Color = string | { r: number?, g: number?, b: number?, a: number? } | { number }
export type ColorTable = { r: number, g: number, b: number, a: number }
export type CanvasPoints = { number } | { { number } | { x: number, y: number } }
export type GradientStop = Color | { offset: number?, color: Color } | { number | Color }
export type GradientStops = { GradientStop }
export type BlendMode = "normal" | "add" | "subtract" | "multiply" | "screen" | "overlay" | "darken" | "lighten" | "difference" | "exclusion"
export type ResizeFilter = "nearest" | "linear" | "cubic" | "gaussian" | "lanczos"
export type ImageFormat = "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "tiff" | "webp" | "qoi" | "tga"
export type MaskMode = "luminance" | "alpha"
export type HistogramChannel = "r" | "g" | "b" | "a" | "luma"
export type Channel = "r" | "g" | "b" | "a"
export type NoiseKind = "perlin" | "value" | "worley" | "ridged"

export type NoiseOptions = {
    seed: number?,
    scale: number?,
    octaves: number?,
    persistence: number?,
    lacunarity: number?,
    kind: NoiseKind?,
}

export type NoiseFillOptions = {
    x: number?,
    y: number?,
    stops: GradientStops?,
    alpha: boolean?,
}

export type NoiseMap = {
    sample: (self: NoiseMap, x: number, y: number) -> number,
    set: (self: NoiseMap, options: NoiseOptions) -> NoiseMap,
    warp: (self: NoiseMap, source: NoiseMap, strength: number) -> NoiseMap,
    unwarp: (self: NoiseMap) -> NoiseMap,
    fill: (self: NoiseMap, target: Canvas, options: NoiseFillOptions?) -> (),
    image: (self: NoiseMap, width: number, height: number, options: NoiseFillOptions?) -> Canvas,
}

export type CanvasFont = {
    measure: (self: CanvasFont, text: string, size: number) -> (number, number),
}

export type Canvas = {
    width: (self: Canvas) -> number,
    height: (self: Canvas) -> number,
    size: (self: Canvas) -> (number, number),
    clone: (self: Canvas) -> Canvas,
    getPixel: (self: Canvas, x: number, y: number) -> ColorTable,
    setPixel: (self: Canvas, x: number, y: number, color: Color) -> Canvas,
    sample: (self: Canvas, x: number, y: number) -> ColorTable,
    pixels: (self: Canvas) -> string,
    buffer: (self: Canvas, x: number?, y: number?, width: number?, height: number?) -> buffer,
    setBuffer: (self: Canvas, data: buffer | string, x: number?, y: number?, width: number?, height: number?) -> Canvas,
    slice: (self: Canvas, tileWidth: number, tileHeight: number) -> { Canvas },
    mapPixels: (self: Canvas, transform: (x: number, y: number, r: number, g: number, b: number, a: number) -> (number?, number?, number?, number?)) -> Canvas,
    clear: (self: Canvas, color: Color?) -> Canvas,
    fill: (self: Canvas, color: Color) -> Canvas,
    floodFill: (self: Canvas, x: number, y: number, color: Color, tolerance: number?) -> Canvas,
    line: (self: Canvas, x1: number, y1: number, x2: number, y2: number, color: Color, thickness: number?) -> Canvas,
    dashedLine: (self: Canvas, x1: number, y1: number, x2: number, y2: number, color: Color, thickness: number?, dash: number?, gap: number?) -> Canvas,
    polyline: (self: Canvas, points: CanvasPoints, color: Color, thickness: number?) -> Canvas,
    arrow: (self: Canvas, x1: number, y1: number, x2: number, y2: number, color: Color, thickness: number?, headSize: number?) -> Canvas,
    star: (self: Canvas, cx: number, cy: number, points: number, outerRadius: number, innerRadius: number, color: Color, rotation: number?) -> Canvas,
    grid: (self: Canvas, spacingX: number, spacingY: number, color: Color, thickness: number?) -> Canvas,
    rect: (self: Canvas, x: number, y: number, width: number, height: number, color: Color) -> Canvas,
    strokeRect: (self: Canvas, x: number, y: number, width: number, height: number, color: Color, thickness: number?) -> Canvas,
    roundedRect: (self: Canvas, x: number, y: number, width: number, height: number, radius: number, color: Color) -> Canvas,
    strokeRoundedRect: (self: Canvas, x: number, y: number, width: number, height: number, radius: number, color: Color, thickness: number?) -> Canvas,
    circle: (self: Canvas, cx: number, cy: number, radius: number, color: Color) -> Canvas,
    strokeCircle: (self: Canvas, cx: number, cy: number, radius: number, color: Color, thickness: number?) -> Canvas,
    ellipse: (self: Canvas, cx: number, cy: number, rx: number, ry: number, color: Color) -> Canvas,
    strokeEllipse: (self: Canvas, cx: number, cy: number, rx: number, ry: number, color: Color) -> Canvas,
    polygon: (self: Canvas, points: CanvasPoints, color: Color) -> Canvas,
    strokePolygon: (self: Canvas, points: CanvasPoints, color: Color, thickness: number?) -> Canvas,
    triangle: (self: Canvas, x1: number, y1: number, x2: number, y2: number, x3: number, y3: number, color: Color) -> Canvas,
    arc: (self: Canvas, cx: number, cy: number, radius: number, startAngle: number, endAngle: number, color: Color, thickness: number?) -> Canvas,
    pie: (self: Canvas, cx: number, cy: number, radius: number, startAngle: number, endAngle: number, color: Color) -> Canvas,
    bezier: (self: Canvas, x1: number, y1: number, cx1: number, cy1: number, cx2: number, cy2: number, x2: number, y2: number, color: Color) -> Canvas,
    linearGradient: (self: Canvas, x1: number, y1: number, x2: number, y2: number, stops: GradientStops) -> Canvas,
    radialGradient: (self: Canvas, cx: number, cy: number, radius: number, stops: GradientStops) -> Canvas,
    conicGradient: (self: Canvas, cx: number, cy: number, startAngle: number?, stops: GradientStops) -> Canvas,
    text: (self: Canvas, text: string, x: number, y: number, size: number, color: Color, font: CanvasFont, options: { spacing: number?, lineHeight: number? }?) -> Canvas,
    drawImage: (self: Canvas, image: Canvas, x: number, y: number, options: { opacity: number?, blend: BlendMode?, width: number?, height: number?, filter: ResizeFilter? }?) -> Canvas,
    merge: (self: Canvas, image: Canvas, blend: BlendMode?, opacity: number?) -> Canvas,
    tile: (self: Canvas, image: Canvas) -> Canvas,
    mask: (self: Canvas, image: Canvas, mode: MaskMode?) -> Canvas,
    dropShadow: (self: Canvas, dx: number, dy: number, sigma: number, color: Color?, strength: number?) -> Canvas,
    glow: (self: Canvas, sigma: number, color: Color?, strength: number?) -> Canvas,
    outline: (self: Canvas, color: Color, radius: number?) -> Canvas,
    resize: (self: Canvas, width: number, height: number, filter: ResizeFilter?) -> Canvas,
    scale: (self: Canvas, factor: number, filter: ResizeFilter?) -> Canvas,
    thumbnail: (self: Canvas, width: number, height: number) -> Canvas,
    fit: (self: Canvas, width: number, height: number, background: Color?, filter: ResizeFilter?) -> Canvas,
    cover: (self: Canvas, width: number, height: number, filter: ResizeFilter?) -> Canvas,
    crop: (self: Canvas, x: number, y: number, width: number, height: number) -> Canvas,
    rotate90: (self: Canvas) -> Canvas,
    rotate180: (self: Canvas) -> Canvas,
    rotate270: (self: Canvas) -> Canvas,
    rotate: (self: Canvas, degrees: number, background: Color?) -> Canvas,
    flipX: (self: Canvas) -> Canvas,
    flipY: (self: Canvas) -> Canvas,
    shift: (self: Canvas, dx: number, dy: number) -> Canvas,
    pad: (self: Canvas, left: number, top: number, right: number, bottom: number, color: Color?) -> Canvas,
    trim: (self: Canvas) -> Canvas,
    grayscale: (self: Canvas) -> Canvas,
    invert: (self: Canvas) -> Canvas,
    sepia: (self: Canvas) -> Canvas,
    gamma: (self: Canvas, gamma: number) -> Canvas,
    exposure: (self: Canvas, stops: number) -> Canvas,
    levels: (self: Canvas, inBlack: number, inWhite: number, outBlack: number?, outWhite: number?) -> Canvas,
    normalize: (self: Canvas) -> Canvas,
    equalize: (self: Canvas) -> Canvas,
    duotone: (self: Canvas, dark: Color, light: Color) -> Canvas,
    temperature: (self: Canvas, amount: number) -> Canvas,
    solarize: (self: Canvas, threshold: number?) -> Canvas,
    channels: (self: Canvas, rFactor: number, gFactor: number, bFactor: number) -> Canvas,
    colorReplace: (self: Canvas, from: Color, to: Color, tolerance: number?) -> Canvas,
    chromaKey: (self: Canvas, color: Color, tolerance: number?, softness: number?) -> Canvas,
    dither: (self: Canvas, levels: number?) -> Canvas,
    scanlines: (self: Canvas, spacing: number?, opacity: number?) -> Canvas,
    halftone: (self: Canvas, cellSize: number, color: Color?, background: Color?) -> Canvas,
    motionBlur: (self: Canvas, angle: number, distance: number) -> Canvas,
    wave: (self: Canvas, amplitudeX: number, wavelengthX: number, amplitudeY: number?, wavelengthY: number?) -> Canvas,
    swirl: (self: Canvas, cx: number, cy: number, radius: number, degrees: number) -> Canvas,
    bulge: (self: Canvas, cx: number, cy: number, radius: number, amount: number) -> Canvas,
    chromaticAberration: (self: Canvas, offset: number) -> Canvas,
    skew: (self: Canvas, xDegrees: number, yDegrees: number, background: Color?) -> Canvas,
    blit: (self: Canvas, source: Canvas, sx: number, sy: number, sw: number, sh: number, dx: number, dy: number) -> Canvas,
    channel: (self: Canvas, channel: Channel) -> Canvas,
    setChannel: (self: Canvas, channel: Channel, source: Canvas) -> Canvas,
    splitChannels: (self: Canvas) -> (Canvas, Canvas, Canvas, Canvas),
    getBounds: (self: Canvas) -> (number?, number?, number?, number?),
    dominantColors: (self: Canvas, count: number?) -> { ColorTable & { count: number } },
    brightness: (self: Canvas, delta: number) -> Canvas,
    contrast: (self: Canvas, factor: number) -> Canvas,
    saturate: (self: Canvas, factor: number) -> Canvas,
    hueRotate: (self: Canvas, degrees: number) -> Canvas,
    tint: (self: Canvas, color: Color, strength: number?) -> Canvas,
    opacity: (self: Canvas, factor: number) -> Canvas,
    blur: (self: Canvas, sigma: number) -> Canvas,
    sharpen: (self: Canvas, sigma: number?, threshold: number?) -> Canvas,
    pixelate: (self: Canvas, blockSize: number) -> Canvas,
    posterize: (self: Canvas, levels: number) -> Canvas,
    threshold: (self: Canvas, level: number?) -> Canvas,
    noise: (self: Canvas, amount: number) -> Canvas,
    vignette: (self: Canvas, strength: number?, color: Color?) -> Canvas,
    emboss: (self: Canvas) -> Canvas,
    edges: (self: Canvas) -> Canvas,
    convolve: (self: Canvas, kernel: { number }, bias: number?) -> Canvas,
    averageColor: (self: Canvas) -> ColorTable,
    histogram: (self: Canvas, channel: HistogramChannel?) -> { number },
    compare: (self: Canvas, other: Canvas) -> number,
    encode: (self: Canvas, format: ImageFormat?, quality: number?) -> string,
    dataUrl: (self: Canvas, format: ImageFormat?) -> string,
}

export type CanvasLib = {
    new: (width: number, height: number, background: Color?) -> Canvas,
    decode: (data: string) -> Canvas,
    fromPixels: (width: number, height: number, rgba: string | buffer) -> Canvas,
    montage: (canvases: { Canvas }, options: { cols: number?, gap: number?, background: Color? }?) -> Canvas,
    checker: (width: number, height: number, size: number?, color1: Color?, color2: Color?) -> Canvas,
    font: (path: string) -> CanvasFont,
    rgb: (r: number, g: number, b: number) -> ColorTable,
    rgba: (r: number, g: number, b: number, a: number) -> ColorTable,
    hsv: (h: number, s: number, v: number, a: number?) -> ColorTable,
    hex: (hex: string) -> ColorTable,
    noise: (options: NoiseOptions?) -> NoiseMap,
    formats: () -> { ImageFormat },
}

export type Random = {
    NextInteger: (self: Random, min: number, max: number) -> number,
    NextNumber: ((self: Random) -> number) & ((self: Random, min: number, max: number) -> number),
    NextBoolean: (self: Random, chance: number?) -> boolean,
    NextBytes: (self: Random, count: number) -> string,
    Shuffle: <T>(self: Random, list: { T }) -> { T },
    Choice: <T>(self: Random, list: { T }) -> T?,
    Clone: (self: Random) -> Random,
}

export type RandomLib = {
    new: (seed: number?) -> Random,
}

export type ForeignLibrary = { [string]: (...any) -> ...any }

export type DllLib = {
    open: (path: string) -> ForeignLibrary,
    load: (path: string) -> ForeignLibrary,
}

export type Task = {
    Cancel: (self: Task) -> (),
    Reschedule: (self: Task, seconds: number) -> (),
    IsCancelled: (self: Task) -> boolean,
    IsFinished: (self: Task) -> boolean,
    Kind: (self: Task) -> string,
}

export type TaskCallback = ((...any) -> ()) | thread

export type TaskLib = {
    wait: (seconds: number?) -> number,
    spawn: (callback: TaskCallback, ...any) -> Task,
    defer: (callback: TaskCallback, ...any) -> Task,
    delay: (seconds: number, callback: TaskCallback, ...any) -> Task,
    schedule: (callback: TaskCallback, seconds: number, ...any) -> Task,
    reschedule: (task: Task, seconds: number) -> (),
    cancel: (target: Task | thread) -> (),
}

export type MemCache = {
    set: (self: MemCache, key: string, value: any, ttl: number?) -> (),
    get: (self: MemCache, key: string) -> any,
    take: (self: MemCache, key: string) -> any,
    add: (self: MemCache, key: string, value: any, ttl: number?) -> boolean,
    increment: (self: MemCache, key: string, delta: number?, ttl: number?) -> number,
    has: (self: MemCache, key: string) -> boolean,
    delete: (self: MemCache, key: string) -> boolean,
    ttl: (self: MemCache, key: string) -> number?,
    touch: (self: MemCache, key: string, ttl: number?) -> boolean,
    keys: (self: MemCache) -> { string },
    count: (self: MemCache) -> number,
    clear: (self: MemCache) -> (),
    name: (self: MemCache) -> string,
    stats: (self: MemCache) -> { hits: number, misses: number, count: number },
}

export type LruCache = {
    set: (self: LruCache, key: string, value: any, ttl: number?) -> (),
    get: (self: LruCache, key: string) -> any,
    peek: (self: LruCache, key: string) -> any,
    has: (self: LruCache, key: string) -> boolean,
    delete: (self: LruCache, key: string) -> boolean,
    keys: (self: LruCache) -> { string },
    count: (self: LruCache) -> number,
    capacity: (self: LruCache) -> number,
    clear: (self: LruCache) -> (),
}

export type MemCacheOptions = {
    ttl: number?,
}

export type CacheLib = {
    mem: ((name: string?, options: MemCacheOptions?) -> MemCache) & ((options: MemCacheOptions) -> MemCache),
    lru: (capacity: number, options: MemCacheOptions?) -> LruCache,
    memoize: (callback: (...any) -> ...any, options: { ttl: number?, capacity: number? }?) -> (...any) -> ...any,
    list: () -> { string },
    drop: (name: string) -> boolean,
}

export type ClassData = { [any]: any }

export type Interface = { [any]: any }

export type ClassInherit = {
    Class: ClassData,
    Public: { string }?,
    Private: { string }?,
    Static: { string }?,
    Metamethods: { string }?,
}

export type ClassSpec = {
    Name: string?,
    Private: { [string]: any }?,
    Public: { [string]: any }?,
    Static: { [string]: any }?,
    Inherits: (ClassData | { ClassData | ClassInherit })?,
    Interfaces: { Interface }?,
    __construct: ((...any) -> ())?,
    [string]: any,
}

export type InterfaceSpec = { string } | { Name: string?, Requires: { string } }

declare function parallel(path: string, ...: any): Port

declare function NewClassData(spec: ClassSpec): ClassData
declare function BuildClassData(class: ClassData, ...: any): any
declare function SuperGet(class: ClassData, key: string): any
declare function Interface(spec: InterfaceSpec): Interface
declare function Implements(value: any, interface: Interface): boolean
declare function GetType(value: any): string

declare __dirname: string
declare __filename: string
declare channel: Port
declare messenger: Messenger
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
declare net: Net
declare random: RandomLib
declare task: TaskLib
declare canvas: CanvasLib
declare cache: CacheLib
declare dll: DllLib

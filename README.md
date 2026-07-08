# Lehua

Lehua is a runtime for Luau. You write Luau, run it right away, and build it into
one small program that runs on its own.

It is written in Rust on top of mlua and tokio.

## Install

You have two options. You can download a prebuilt program from the releases, or
you can build it from source.

### Option 1: Download a release

1. Open the Releases page for this project.
2. Download the file for your system:
   - Windows: `lehua-windows-x86_64.exe`
   - Linux: `lehua-linux-x86_64`
3. Rename it to `lehua`, or `lehua.exe` on Windows.
4. On Linux, make it runnable with `chmod +x lehua`.
5. Move it to a folder on your PATH so you can run it from anywhere.

### Option 2: Build from source

First install the tools you need.

- Rust, through rustup. Get it from https://rustup.rs. This gives you cargo,
  which is the Rust build tool.
- A C and C++ compiler. Lehua builds the Luau core from source, so this is
  required.
  - Windows: install the Visual Studio Build Tools and pick the "Desktop
    development with C++" workload.
  - Linux: install gcc and g++. On Debian or Ubuntu that is
    `sudo apt install build-essential`.

Then clone the project and build it.

```
git clone https://github.com/lehua-lang/lehua
cd lehua
cargo build --release
```

The first build takes a few minutes, because the Luau core is compiled from
source. Later builds are much faster.

When it finishes, the program is here:

- Windows: `target\release\lehua.exe`
- Linux: `target/release/lehua`

Copy that file to a folder on your PATH. For example, on Linux:

```
sudo cp target/release/lehua /usr/local/bin/lehua
```

Check that it works:

```
lehua --help
```

## Use

Make a new project:

```
lehua init myapp
cd myapp
```

Run it:

```
lehua run
```

Build a single program you can share. This writes a standalone file into the
`dist` folder that runs on its own, with no Rust and no source files needed:

```
lehua build
```

Remove the build output:

```
lehua clean
```

## Docs

The full guides and library reference live in the `docs` folder. Open
`docs/index.html` in a browser, or turn on GitHub Pages for the `docs` folder.

## Making a release (for maintainers)

1. Set the version in `Cargo.toml`, under `[package]`.
2. Open the Actions tab, pick the `Release` workflow, and run it.

The workflow builds the program for Windows and Linux, then publishes a release
with those files attached. The release tag and title both use the version from
`Cargo.toml`, for example `v0.1.0`.

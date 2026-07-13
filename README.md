# Lehua

-# I am not the best writer so I will say Claude was used to write the docs and this readme. I feel it is important to state any AI use. Code however is by me with very little AI assitance.

Lehua is a runtime for Luau. You write Luau, run it right away, and build it into
one small program that runs on its own.

It is written in Rust on top of mlua and tokio.

## Install

You have three options. You can install it with Rokit, download a prebuilt
program from the releases, or build it from source.

### Option 1: Install with Rokit

If you use [Rokit](https://github.com/rojo-rbx/rokit) to manage your tools,
install Lehua with one command:

```
rokit add thekingofspace/LehuaLang lehua
```

The `lehua` at the end sets the name of the command. Without it, the command
would be called `LehuaLang`.

Run that inside a project to add Lehua to its `rokit.toml`, or add `--global`
to make the command available everywhere:

```
rokit add --global thekingofspace/LehuaLang lehua
```

### Option 2: Download a release

1. Open the Releases page for this project.
2. Download the archive for your system:
   - Windows: `lehua-windows-x86_64.zip`
   - Linux: `lehua-linux-x86_64.tar.gz`
3. Extract it. Inside is a folder with the program: `lehua.exe` on Windows,
   `lehua` on Linux.
4. Move the program to a folder on your PATH so you can run it from anywhere.

### Option 3: Build from source

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
git clone https://github.com/thekingofspace/LehuaLang
cd LehuaLang
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

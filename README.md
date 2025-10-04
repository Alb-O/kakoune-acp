A template Rust project with fully functional and no-frills Nix support, as well as builtin VSCode configuration to get IDE experience without any manual setup (just [install direnv](https://nixos.asia/en/direnv), open in VSCode and accept the suggestions). It uses [crane](https://crane.dev/), via [rust-flake](https://github.com/juspay/rust-flake).

> [!NOTE]
> If you are looking for the original template based on [this blog post](https://srid.ca/rust-nix)'s use of `crate2nix`, browse from [this tag](https://github.com/srid/kakoune-acp/tree/crate2nix). The evolution of this template can be gleaned from [releases](https://github.com/srid/kakoune-acp/releases).

## Usage

You can use [omnix](https://omnix.page/om/init.html)[^omnix] to initialize this template:
```
nix run nixpkgs#omnix -- init github:srid/kakoune-acp -o ~/my-rust-project
```

[^omnix]: If initializing manually, make sure to:
    - Change `name` in Cargo.toml.
    - Run `cargo generate-lockfile` in the nix shelld

## Adapting this template

- There are two CI workflows, and one of them uses Nix which is slower (unless you configure a cache) than the other one based on rustup. Pick one or the other depending on your trade-offs.

## Development (Flakes)

This repo uses [Flakes](https://nixos.asia/en/flakes) from the get-go.

```bash
# Dev shell
nix develop

# or run via cargo
nix develop -c cargo run

# build
nix build
```

We also provide a [`justfile`](https://just.systems/) for Makefile'esque commands to be run inside of the devShell.

## Kakoune Agent Client Protocol daemon

This project now ships with a small utility that bridges the [Agent Client Protocol](https://agentclientprotocol.com/) into a Kakoune editing session. The binary exposes three subcommands:

### 1. Start the daemon

```bash
kakoune-acp daemon \
  --socket /tmp/kakoune-acp.sock \
  --cwd "$PWD" \
  -- path/to/agent --arg value
```

The daemon spawns your ACP agent, establishes the protocol handshake, and listens for client commands on the provided Unix domain socket. The working directory is forwarded to the agent when creating the initial session.

### 2. Send prompts from Kakoune (or the shell)

```bash
kakoune-acp prompt \
  --socket /tmp/kakoune-acp.sock \
  --prompt "Summarise the current buffer" \
  --context-file ~/notes/outline.txt \
  --output plain
```

The `prompt` subcommand collects the agent's streamed updates, renders them into a human friendly transcript, and can optionally emit Kakoune commands (`--output kak-commands`) or send them directly back to the editor (`--send-to-kak`). When invoked from `%sh{}` the current `kak_session` and `kak_client` environment variables are honoured automatically.

You can add additional context snippets inline (`--context "Consider the TODO list"`) or from files (`--context-file notes.md`). The prompt text can also be supplied via `--prompt-file` or piped in through stdin when neither flag is used.

### 3. Inspect or stop the daemon

```bash
# Check health
kakoune-acp status --socket /tmp/kakoune-acp.sock --json

# Gracefully terminate
kakoune-acp shutdown --socket /tmp/kakoune-acp.sock
```

These helpers make it easy to wire the ACP integration into Kakoune commands or external scripts while keeping the agent process alive between prompt turns.

## Tips

- Run `nix flake update` to update all flake inputs.
- Run `nix --accept-flake-config run github:juspay/omnix ci` to build _all_ outputs.
- [pre-commit] hooks will automatically be setup in Nix shell. You can also run `pre-commit run -a` manually to run the hooks (e.g.: to autoformat the project tree using `rustfmt`, `nixpkgs-fmt`, etc.).

## Discussion

- [Zulip](https://nixos.zulipchat.com/#narrow/stream/413950-nix)

## See Also

- [nixos.wiki: Packaging Rust projects with nix](https://nixos.wiki/wiki/Rust#Packaging_Rust_projects_with_nix)

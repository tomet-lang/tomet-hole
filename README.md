<!-- Generated from tmtroot/readme.tmt. Edit that, then `tomet export .`. -->

# Tomet Hole

Pure hole-based pattern matching engine (`hole-engine`) and its Tomet `Hole` (`<?...>`) AST bridge (`tomet-hole`).

## Workspace Layout

- `crates/hole-engine` (`hole-engine`) -- Pure, `tomet`-independent pattern matching engine (regex replacement). Layer 0.
- `crates/tomet-hole` (`tomet-hole`) -- Bridge between `tomet-ast` `Hole` nodes and `hole-engine`. Layer 1.

## Building and Testing

```bash
just build
just test
just clippy
just docs-check
```


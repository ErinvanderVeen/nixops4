
# NixOps4

This is the successor of NixOps 2.
After a development hiatus in the original project, we have decided to fix structural issues through a first-principles rewrite.

**Status: work in progress, not usable.**

## Hacking

The following will open a shell with dependencies, and install pre-commit for automatic formatting.

```console
$ nix develop
```

### VSCode

#### rust-analyzer

If the rust-analyzer extension fails, make sure the devShell was loaded into VSCode via Nix Env Selector or direnv.

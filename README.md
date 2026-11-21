# limon 🍋

Expand Luau require aliases and inject `_G` environment variables from `limon.toml`.

```sh
limon process src --output dist [--clean] [--strict]
```

```toml
output = "dist"
clean = true
strict = true

[aliases]
pkg = "@game/ReplicatedStorage/Packages"

[env]
cast = true
dead_code_elimination = true
preserve_lines = true
required = ["DEV", "PORT"]

[env.types]
DEV = "bool"
PORT = "number"

[env.defaults]
DEV = false
```

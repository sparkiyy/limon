# limon 🍋

Herramienta mínima y rápida en Rust para expandir aliases de `require` en proyectos Luau (como Roblox u otros entornos).

Utiliza únicamente [`full-moon`](https://github.com/Kampfkarren/full-moon) para parsear el código Luau sin pérdida de formato, comentarios o trivia.

## Características

- 🔍 **Resolución por archivo de `.luaurc`**: Busca el archivo `.luaurc` correspondiente a cada archivo Luau subiendo por el árbol de directorios. Si un archivo no tiene `.luaurc`, no es modificado.
- 🛡️ **Respeta `@game` de Roblox**: El alias por defecto `@game` se ignora automáticamente.
- ⚙️ **Configuración vía `limon.toml`**: Define expansiones personalizadas y la carpeta de salida predeterminada.
- 🚀 **Subcomandos incluidos**:
  - `limon process`: Procesa los archivos una sola vez.
  - `limon watch`: Observa el directorio en tiempo real y procesa los cambios incrementalmente.
- 📁 **Soporte `--output`**: Puedes especificar una carpeta de salida mediante el argumento `-o` / `--output` o en `limon.toml`. Si no se especifica, procesa in-place.
- 📦 **Mínimas dependencias**: Mantiene el árbol de dependencias lo más ligero posible.

---

## Configuración

### 1. `.luaurc` (en tu proyecto Luau)

Define los aliases reconocidos por el compilador o linter de Luau:

```json
{
  "aliases": {
    "pkg": "src/shared/Packages",
    "utils": "src/shared/Utils"
  }
}
```

### 2. `limon.toml` (configuración de limon)

Define cómo expandir esos aliases a sus rutas de runtime:

```toml
# Carpeta de salida opcional (si no se pasa --output por CLI)
output = "dist"

[aliases]
pkg = "@game/ReplicatedStorage/Packages"
utils = "@game/ReplicatedStorage/Utils"
```

---

## Uso

```bash
# Procesar una sola vez indicando la carpeta de salida
limon process src --output dist

# O usando el alias corto -o
limon process -o dist

# Si limon.toml tiene `output = "dist"`, basta con:
limon process

# Modo watch (observar y procesar cambios automáticamente)
limon watch src --output dist

# Watch usando configuración de limon.toml
limon watch
```

---

## Ejemplo de Transformación

Código original (`src/client/init.luau`):

```lua
-- Preserva comentarios y estructura
local Roact = require("@pkg/Roact")
local Math = require("@utils/Math").round
local LocalPkg = require "@pkg/DirectString"
local DefaultGame = require("@game/ReplicatedStorage/Config") -- ignorado
```

Código resultante (`dist/client/init.luau`):

```lua
-- Preserva comentarios y estructura
local Roact = require("@game/ReplicatedStorage/Packages/Roact")
local Math = require("@game/ReplicatedStorage/Utils/Math").round
local LocalPkg = require "@game/ReplicatedStorage/Packages/DirectString"
local DefaultGame = require("@game/ReplicatedStorage/Config") -- ignorado
```

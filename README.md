## rojo-build-noalias

A wrapper around the [Rojo](https://github.com/rojo-rbx/rojo) *build* command written in Luau.

## Motivation

Luau LSP does not currently support string requires that map based on the `sourcemap.json` file.

A workaround is to use aliases, but they are not currently supported in Roblox.

This tool replaces aliases with relative string requires based on your project's sourcemap.

## Showcase

add video here

## Installation

### With [Rokit](https://github.com/rojo-rbx/rokit)

```sh
rokit add zilibobi/rojo-build-noalias
```

### Manual

1. Download the latest build from [releases](https://github.com/zilibobi/rojo-build-noalias/releases/latest)
2. Add it to your PATH

## Limitations

- Doesn't work with `rojo serve`
- Doesn't support the rojo `--plugin` option
- Only works with projects that build to a model
- Only supports models with 1 root instance

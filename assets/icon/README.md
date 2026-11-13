# Fleetglass icon

A lens over a honeycomb of clusters: the "glass" over your "fleet".

- `fleetglass.svg`: source. Edit this one.
- `fleetglass-1024.png`: 1024 px render, input for icon generation.

The Tauri app icons in `crates/app/icons/` are generated from the PNG:

```sh
rsvg-convert -w 1024 -h 1024 assets/icon/fleetglass.svg -o assets/icon/fleetglass-1024.png
ui/node_modules/.bin/tauri icon assets/icon/fleetglass-1024.png -o crates/app/icons
```

pixelview 3D format samples
============================================================
The same model (Blender's "Suzanne" monkey) exported to every 3D format
pixelview can VIEW as geometry, plus a hand-written COLLADA cube. Enable
Preferences → Format plugins → "3D models", then open any of these — each
opens the interactive viewer (drag = orbit, wheel = zoom, right-click = FPS fly).

  suzanne.obj    Wavefront OBJ (+ .mtl)   — text, most common interchange
  suzanne.stl    STL                      — 3D-printing (no colour/UV)
  suzanne.ply    Stanford PLY (binary)    — 3D-scanning (our own PLY loader)
  suzanne.gltf   glTF 2.0 (+ .bin)        — modern standard, JSON + binary buffer
  suzanne.glb    glTF binary (GLB)        — modern standard, single self-contained file
  cube.dae       COLLADA (DAE)            — older interchange XML

NOT supported for geometry (shown as a placeholder / not at all):
  .blend  — Blender's own format (right-click → Render with Blender instead)
  .fbx    — proprietary; no reliable pure-Rust reader

Note: our viewer shades geometry + a diffuse texture with one key light — it
does NOT do specular / bump / PBR (see ../materials/README.txt).

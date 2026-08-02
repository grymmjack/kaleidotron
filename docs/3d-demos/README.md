# 3D demos

Sample assets for exploring pixelview's **3D models** plugin
(Preferences → Format plugins → ☑ 3D models).

- **`materials/`** — diverse Wavefront `.mtl` files (+ procedural textures in
  `tex/`). Each `newmtl` renders as a lit "material ball" (diffuse `Kd` colour +
  `map_Kd` texture). See `materials/README.txt` for the list and a note on which
  `.mtl` properties are / aren't visualised (we shade diffuse only — no
  specular / bump / PBR).
- **`models/`** — the same model (Blender's *Suzanne*) exported to every format
  we can view as geometry: OBJ, STL, PLY, glTF, GLB, plus a COLLADA cube. See
  `models/README.txt`. `.blend` / `.fbx` are **not** loadable as geometry
  (right-click a `.blend` → *Render with Blender* instead).

Open this folder in pixelview to see `.mtl` material previews in the grid and
open any model in the interactive viewer (drag = orbit, wheel = zoom,
right-click = FPS fly).

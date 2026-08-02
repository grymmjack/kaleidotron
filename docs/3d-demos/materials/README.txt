pixelview .mtl material demos
============================================================
Our .mtl preview renders ONE lit "material ball" per `newmtl`, using only:
  • Kd       diffuse COLOR        → the ball's colour
  • map_Kd   diffuse TEXTURE      → wrapped onto the ball
Lit by a single key light with flat (matte / Lambertian) diffuse shading.

It is a "what colour/texture is this material" preview — NOT a physically
based render. These common material terms are PARSED by Blender & friends
but are NOT visualised here:
  Ns / Ks .......... shininess / specular highlight   (no highlights shown)
  Ka ............... ambient colour                    (fixed ambient used)
  d / Tr ........... transparency / opacity            (always opaque)
  illum ............ illumination model                (ignored)
  map_Bump / norm .. bump / normal maps                (no surface relief)
  Pm / Pr / Ps ..... PBR metalness / roughness / sheen (ignored)
  subsurface ....... (not a Wavefront .mtl field at all)

So "gold"/"steel" here are just COLOURS, not shiny metal. Open these in a
3D DCC (Blender) to see specular/bump/etc.

Files (multi-material = a grid of balls in one tile; single = one big ball)
------------------------------------------------------------
  01-rainbow.mtl     12 hues around the colour wheel
  02-grayscale.mtl   black → white ramp
  03-pastels.mtl     soft tints
  04-jewels.mtl      rich saturated jewel tones
  05-earth.mtl       natural browns/greens
  06-neon.mtl        bright saturated
  07-metals.mtl      metal-ish colours (COLOUR only — no specular here)
  08-textured.mtl    one ball per texture in tex/ (map_Kd)
  09-mixed.mtl       solids + textures together
  10..13-single-*.mtl  one material each (a single big ball)
  tex/               13 procedural textures the map_Kd demos reference

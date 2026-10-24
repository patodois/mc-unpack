#!/bin/sh
# Exports a few blocks and items with mc-unpack and renders them, with the raw textures of the jar beside each.
#   sh examples.sh <output folder> [minecraft version]      (needs `mc-unpack` on PATH, npm install done here, CHROMIUM or Playwright's browser)
set -e
OUT=${1:?output folder}
V=${2:-26.3}
HERE=$(cd "$(dirname "$0")" && pwd)
GL="$OUT/gltf"; REF="$OUT/ref"; PNG="$OUT/png"
mkdir -p "$GL" "$REF" "$PNG"
M="mc-unpack --mc-version=$V"
shot() { # name gltf refs [view]
  REFS="$3" node "$HERE/shot.cjs" "$PNG/$1.png" "$GL/$2.gltf" "${4:-fit=1.55&dir=0.7,0.6,1&look=mc&bg=9db4c8}" 760x600
}
for t in block/grass_block_top block/grass_block_side block/grass_block_side_overlay block/dirt block/oak_planks block/oak_leaves; do
  $M texture "$t" -o "$REF" > /dev/null
done
$M model block/grass_block -o "$GL" --name grass_plain
$M model block/grass_block -o "$GL" --name grass_tinted --tint 0=grass@plains
$M model "block/oak_stairs?variant=facing=east,half=bottom,shape=straight" -o "$GL" --name stairs_east
$M model block/oak_leaves -o "$GL" --name leaves_plains --tint 0=foliage@plains
$M model item/diamond_sword -o "$GL" --name sword
$M model block/chest -o "$GL" --name chest || echo "block/chest is left out, as expected"
G="$REF/block_grass_block_top.png,$REF/block_grass_block_side.png,$REF/block_grass_block_side_overlay.png,$REF/block_dirt.png"
shot grass_plain grass_plain "$G"
shot grass_tinted grass_tinted "$G"
shot stairs_east stairs_east "$REF/block_oak_planks.png"
shot leaves_plains leaves_plains "$REF/block_oak_leaves.png"
shot sword sword "" "fit=1.3&dir=0.4,0.2,1&look=mc&bg=9db4c8"

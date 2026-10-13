#include "../../common/dsl.h"
#include "../../common/defines.h"
#include "../common/mxu_fragment/ops.h"

using namespace metal;

template <uint BITS>
VARIANTS(BITS, 4, 8)
KERNEL(GemmTileCodes)(
    const device uint8_t* codes,
    device uint8_t* tiled,
    const constant uint& row_count,
    const constant uint& row_bytes,
    const constant uint& row_block_count,
    const constant uint& chunk_count,
    const uint chunk GROUPS(chunk_count),
    const uint row_block GROUPS(row_block_count),
    const uint lane THREADS(METAL_SIMD_SIZE)
) {
  using Ops = uzu::matmul::MxuFragmentOps<>;
  constexpr uint CHUNK_BYTES = BITS == 4 ? 16 : 32;
  constexpr ushort COL_FRAGMENTS = BITS == 4 ? 1 : 2;
  constexpr uint LANE_WORDS = 2 * Ops::THREAD_ELEMENT_ROWS * COL_FRAGMENTS;
  const short2 position = Ops::get_position(ushort(lane));
  device uint* out = reinterpret_cast<device uint*>(
      tiled + ((size_t(row_block) * chunk_count + chunk) * METAL_SIMD_SIZE + lane) * LANE_WORDS * sizeof(uint)
  );
  for (ushort fragment_row = 0; fragment_row < 2; ++fragment_row) {
    for (ushort row_slot = 0; row_slot < Ops::THREAD_ELEMENT_ROWS; ++row_slot) {
      const uint row = row_block * 2 * Ops::FRAGMENT_ROWS + uint(position.y) + fragment_row * Ops::FRAGMENT_ROWS +
                       row_slot * Ops::THREAD_ELEMENT_ROW_STRIDE;
      for (ushort fragment_column = 0; fragment_column < COL_FRAGMENTS; ++fragment_column) {
        uint word = 0;
        if (row < row_count) {
          word = *reinterpret_cast<const device uint*>(
              codes + size_t(row) * row_bytes + chunk * CHUNK_BYTES + uint(position.x) +
              fragment_column * Ops::FRAGMENT_COLS
          );
        }
        out[(fragment_row * Ops::THREAD_ELEMENT_ROWS + row_slot) * COL_FRAGMENTS + fragment_column] = word;
      }
    }
  }
}

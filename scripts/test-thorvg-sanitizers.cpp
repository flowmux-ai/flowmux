// SPDX-License-Identifier: GPL-3.0-or-later
// Reproduce the viewer's C API sequence using upstream animation fixtures.
#include <thorvg_capi.h>
#include <cassert>
#include <cstdio>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

int main(int argc, char** argv) {
    assert(argc == 2);
    std::ifstream input(argv[1], std::ios::binary);
    std::string data((std::istreambuf_iterator<char>(input)), {});
    assert(!data.empty() && data.size() < 16 * 1024 * 1024);
    assert(tvg_engine_init(0) == TVG_RESULT_SUCCESS);
    auto* animation = tvg_animation_new();
    assert(animation);
    auto* picture = tvg_animation_get_picture(animation);
    assert(picture);
    assert(tvg_picture_load_data(picture, data.data(), data.size(), "lottie+json", nullptr, true) == TVG_RESULT_SUCCESS);
    assert(tvg_picture_set_size(picture, 640, 480) == TVG_RESULT_SUCCESS);
    auto* canvas = tvg_swcanvas_create(TVG_ENGINE_OPTION_NONE);
    assert(canvas);
    std::vector<uint32_t> pixels(640 * 480);
    assert(tvg_swcanvas_set_target(canvas, pixels.data(), 640, 640, 480, TVG_COLORSPACE_ABGR8888) == TVG_RESULT_SUCCESS);
    assert(tvg_canvas_add(canvas, picture) == TVG_RESULT_SUCCESS);
    float count = 0;
    assert(tvg_animation_get_total_frame(animation, &count) == TVG_RESULT_SUCCESS);
    std::printf("fixture=%s frames=%g\n", argv[1], count);
    std::fflush(stdout);
    for (float frame = 0; frame < count; frame += 1.0f) {
        auto result = tvg_animation_set_frame(animation, frame);
        assert(result == TVG_RESULT_SUCCESS || result == TVG_RESULT_INSUFFICIENT_CONDITION);
        assert(tvg_canvas_update(canvas) == TVG_RESULT_SUCCESS);
        assert(tvg_canvas_draw(canvas, true) == TVG_RESULT_SUCCESS);
        assert(tvg_canvas_sync(canvas) == TVG_RESULT_SUCCESS);
    }
    assert(tvg_canvas_destroy(canvas) == TVG_RESULT_SUCCESS);
    assert(tvg_animation_del(animation) == TVG_RESULT_SUCCESS);
    assert(tvg_engine_term() == TVG_RESULT_SUCCESS);
}

#include "rk_tts_c_api.h"

// Ensure <string> is included before rk_tts_rknn.h to work around
// GCC 14 header ordering issue (deque<std::string> needs complete type).
#include <string>
#include "rk_tts_rknn.h"

struct InstanceContext {
    rktts_audio_callback cb;
    void *userdata;
};

struct RkTtsHandle {
    rk_tts_interface *tts;
    InstanceContext *ctx;
};

static void callback_bridge(const int16_t *data, int len, bool is_last, void *userdata) {
    InstanceContext *ctx = static_cast<InstanceContext *>(userdata);
    ctx->cb(data, len, is_last ? 1 : 0, ctx->userdata);
}

extern "C" {

void *rktts_create(void) {
    RkTtsHandle *h = new RkTtsHandle{new rk_tts_interface(), nullptr};
    return h;
}

int rktts_init(void *handle, const char *auth_json, const char *model_path,
               const char *dicts_path, int speaker_id, float alpha,
               int sample_rate, rktts_audio_callback cb, void *userdata) {
    RkTtsHandle *h = static_cast<RkTtsHandle *>(handle);
    h->ctx = new InstanceContext{cb, userdata};
    return h->tts->init(auth_json, model_path, dicts_path, speaker_id,
                         alpha, sample_rate, callback_bridge, h->ctx);
}

int rktts_inference(void *handle, const char *text) {
    RkTtsHandle *h = static_cast<RkTtsHandle *>(handle);
    return h->tts->inference(text);
}

int rktts_release(void *handle) {
    RkTtsHandle *h = static_cast<RkTtsHandle *>(handle);
    int ret = h->tts->release();
    if (h->ctx) {
        delete h->ctx;
        h->ctx = nullptr;
    }
    return ret;
}

void rktts_destroy(void *handle) {
    RkTtsHandle *h = static_cast<RkTtsHandle *>(handle);
    if (h->ctx) {
        h->tts->release();
        delete h->ctx;
    }
    delete h->tts;
    delete h;
}

}

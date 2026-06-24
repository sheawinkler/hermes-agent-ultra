#ifndef RK_TTS_C_API_H
#define RK_TTS_C_API_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*rktts_audio_callback)(const int16_t *data, int len, int is_last, void *userdata);

void* rktts_create(void);
int   rktts_init(void *handle, const char *auth_json, const char *model_path,
                 const char *dicts_path, int speaker_id, float alpha,
                 int sample_rate, rktts_audio_callback cb, void *userdata);
int   rktts_inference(void *handle, const char *text);
int   rktts_release(void *handle);
void  rktts_destroy(void *handle);

#ifdef __cplusplus
}
#endif

#endif

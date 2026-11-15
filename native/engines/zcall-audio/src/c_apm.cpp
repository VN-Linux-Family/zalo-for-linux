#include <cstdint>
#include <vector>

#ifdef HAS_WEBRTC_APM
#include <modules/audio_processing/include/audio_processing.h>

extern "C" {

struct ZcallApm {
    webrtc::AudioProcessing* apm;
    webrtc::StreamConfig config;
    int sample_rate;
    int channels;
};

void* zcall_apm_create(int sample_rate_hz, int channels, int aec_enabled, int ns_enabled, int hpf_enabled) {
    webrtc::AudioProcessing* apm = webrtc::AudioProcessingBuilder().Create();
    if (!apm) {
        return nullptr;
    }

    webrtc::AudioProcessing::Config config;
    config.echo_canceller.enabled = (aec_enabled != 0);
    config.echo_canceller.mobile_mode = false;
    config.high_pass_filter.enabled = (hpf_enabled != 0);
    config.noise_suppression.enabled = (ns_enabled != 0);
    config.noise_suppression.level = webrtc::AudioProcessing::Config::NoiseSuppression::kHigh;
    config.gain_controller1.enabled = false;

    apm->ApplyConfig(config);

    ZcallApm* ctx = new ZcallApm();
    ctx->apm = apm;
    ctx->config = webrtc::StreamConfig(sample_rate_hz, channels);
    ctx->sample_rate = sample_rate_hz;
    ctx->channels = channels;
    return ctx;
}

int zcall_apm_process_render(void* handle, const int16_t* pcm, int num_samples) {
    if (!handle || !pcm) {
        return -1;
    }
    ZcallApm* ctx = static_cast<ZcallApm*>(handle);
    int chunk_size = ctx->sample_rate / 100; // 160 for 16kHz (10 ms)
    std::vector<int16_t> scratch_dest(chunk_size, 0);
    for (int offset = 0; offset + chunk_size <= num_samples; offset += chunk_size) {
        ctx->apm->ProcessReverseStream(pcm + offset, ctx->config, ctx->config, scratch_dest.data());
    }
    return 0;
}

int zcall_apm_process_capture(void* handle, int16_t* pcm, int num_samples, int delay_ms) {
    if (!handle || !pcm) {
        return -1;
    }
    ZcallApm* ctx = static_cast<ZcallApm*>(handle);
    if (delay_ms >= 0) {
        ctx->apm->set_stream_delay_ms(delay_ms);
    }
    int chunk_size = ctx->sample_rate / 100; // 160 for 16kHz (10 ms)
    for (int offset = 0; offset + chunk_size <= num_samples; offset += chunk_size) {
        ctx->apm->ProcessStream(pcm + offset, ctx->config, ctx->config, pcm + offset);
    }
    return 0;
}

void zcall_apm_destroy(void* handle) {
    if (!handle) {
        return;
    }
    ZcallApm* ctx = static_cast<ZcallApm*>(handle);
    delete ctx->apm;
    delete ctx;
}

} // extern "C"

#else // !HAS_WEBRTC_APM

extern "C" {

void* zcall_apm_create(int, int, int, int, int) {
    return nullptr;
}

int zcall_apm_process_render(void*, const int16_t*, int) {
    return 0;
}

int zcall_apm_process_capture(void*, int16_t*, int, int) {
    return 0;
}

void zcall_apm_destroy(void*) {
}

} // extern "C"

#endif // HAS_WEBRTC_APM

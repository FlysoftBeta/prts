#include <vpl/mfxdispatcher.h>
#include <vpl/mfxvideo.h>
#include <cstdio>
#include <cstdint>
#include <d3d11.h>
#include <wrl/client.h>
#include <memory>
#include <vector>
#include <unordered_map>
#include <algorithm>
#include <cstring>

namespace {
struct Loader {
    mfxLoader value = MFXLoad();
    ~Loader() { if (value) MFXUnload(value); }
};
int message(char* output, uint32_t capacity, const char* stage, int status) {
    if (output && capacity) std::snprintf(output, capacity, "%s: mfxStatus=%d", stage, status);
    return status;
}
mfxStatus filter(mfxLoader loader, const char* property, mfxU32 value) {
    mfxConfig config = MFXCreateConfig(loader);
    if (!config) return MFX_ERR_MEMORY_ALLOC;
    mfxVariant variant{};
    variant.Type = MFX_VARIANT_TYPE_U32;
    variant.Data.U32 = value;
    return MFXSetConfigFilterProperty(config, reinterpret_cast<const mfxU8*>(property), variant);
}
using Microsoft::WRL::ComPtr;
struct Surface { ComPtr<ID3D11Texture2D> texture; };
struct Allocation {
    std::vector<std::unique_ptr<Surface>> surfaces;
    std::vector<mfxMemId> ids;
};
struct Allocator {
    ComPtr<ID3D11Device> device;
    std::unordered_map<mfxMemId*, std::unique_ptr<Allocation>> allocations;
    mfxFrameAllocator callbacks{};
    explicit Allocator(ID3D11Device* d) : device(d) {
        callbacks.pthis = this;
        callbacks.Alloc = alloc;
        callbacks.Free = free;
        callbacks.Lock = lock;
        callbacks.Unlock = unlock;
        callbacks.GetHDL = handle;
    }
    static mfxStatus MFX_CDECL alloc(mfxHDL context, mfxFrameAllocRequest* request, mfxFrameAllocResponse* response) {
        if (!context || !request || !response) return MFX_ERR_NULL_PTR;
        if (request->Info.FourCC != MFX_FOURCC_NV12 || !request->NumFrameSuggested || request->NumFrameSuggested > 64 ||
            !request->Info.Width || !request->Info.Height || request->Info.Width > 4096 || request->Info.Height > 4096) {
            std::fprintf(stderr, "oneVPL unsupported allocation FourCC=%x type=%x count=%u size=%ux%u\n", request->Info.FourCC, request->Type, request->NumFrameSuggested, request->Info.Width, request->Info.Height);
            return MFX_ERR_UNSUPPORTED;
        }
        try {
            auto self = static_cast<Allocator*>(context);
            auto allocation = std::make_unique<Allocation>();
            allocation->ids.reserve(request->NumFrameSuggested);
            for (unsigned i = 0; i < request->NumFrameSuggested; ++i) {
                auto surface = std::make_unique<Surface>();
                D3D11_TEXTURE2D_DESC desc{};
                desc.Width = request->Info.Width;
                desc.Height = request->Info.Height;
                desc.MipLevels = desc.ArraySize = desc.SampleDesc.Count = 1;
                desc.Format = DXGI_FORMAT_NV12;
                desc.Usage = D3D11_USAGE_DEFAULT;
                desc.BindFlags = D3D11_BIND_DECODER;
                if ((request->Type & MFX_MEMTYPE_VIDEO_MEMORY_ENCODER_TARGET) && (request->Type & MFX_MEMTYPE_INTERNAL_FRAME)) desc.BindFlags |= D3D11_BIND_VIDEO_ENCODER;
                if (FAILED(self->device->CreateTexture2D(&desc, nullptr, &surface->texture))) return MFX_ERR_MEMORY_ALLOC;
                allocation->ids.push_back(surface.get());
                allocation->surfaces.push_back(std::move(surface));
            }
            response->mids = allocation->ids.data();
            response->NumFrameActual = static_cast<mfxU16>(allocation->ids.size());
            self->allocations.emplace(response->mids, std::move(allocation));
            return MFX_ERR_NONE;
        } catch (...) { return MFX_ERR_MEMORY_ALLOC; }
    }
    static mfxStatus MFX_CDECL free(mfxHDL context, mfxFrameAllocResponse* response) {
        if (!context || !response) return MFX_ERR_NULL_PTR;
        static_cast<Allocator*>(context)->allocations.erase(response->mids);
        response->mids = nullptr;
        response->NumFrameActual = 0;
        return MFX_ERR_NONE;
    }
    static mfxStatus MFX_CDECL lock(mfxHDL, mfxMemId, mfxFrameData*) { return MFX_ERR_UNSUPPORTED; }
    static mfxStatus MFX_CDECL unlock(mfxHDL, mfxMemId, mfxFrameData*) { return MFX_ERR_UNSUPPORTED; }
    static mfxStatus MFX_CDECL handle(mfxHDL, mfxMemId id, mfxHDL* output) {
        if (!id || !output) return MFX_ERR_NULL_PTR;
        auto surface = static_cast<Surface*>(id);
        auto pair = reinterpret_cast<mfxHDLPair*>(output);
        // Same borrowed texture/subresource contract as Intel's sample allocator.
        pair->first = surface->texture.Get();
        pair->second = nullptr;
        return MFX_ERR_NONE;
    }
};
}

namespace {
int open_session(mfxLoader loader, void* device, mfxSession* opened, char* output, uint32_t capacity) {
    if (!loader) return message(output, capacity, "MFXLoad", MFX_ERR_MEMORY_ALLOC);
    struct Property { const char* name; mfxU32 value; };
    const Property properties[] = {
        {"mfxImplDescription.Impl", MFX_IMPL_TYPE_HARDWARE},
        {"mfxImplDescription.VendorID", 0x8086},
        {"mfxImplDescription.AccelerationMode", MFX_ACCEL_MODE_VIA_D3D11},
    };
    for (const auto& property : properties) {
        auto status = filter(loader, property.name, property.value);
        if (status < 0) return message(output, capacity, property.name, status);
    }
    mfxSession session{};
    auto status = MFXCreateSession(loader, 0, &session);
    if (status < 0) return message(output, capacity, "MFXCreateSession Intel D3D11", status);
    status = MFXVideoCORE_SetHandle(session, MFX_HANDLE_D3D11_DEVICE, device);
    if (status < 0) { MFXClose(session); return message(output, capacity, "SetHandle D3D11", status); }
    mfxVersion version{};
    status = MFXQueryVersion(session, &version);
    mfxIMPL implementation{};
    if (status >= 0) status = MFXQueryIMPL(session, &implementation);
    if (status >= 0 && output && capacity) std::snprintf(output, capacity, "Intel oneVPL dispatcher2.17 runtimeAPI=%u.%u implementation=0x%x D3D11 device accepted", version.Major, version.Minor, implementation);
    if (status < 0) { MFXClose(session); return message(output, capacity, "QueryVersion/IMPL", status); }
    *opened = session;
    return 0;
}
struct Failure { const char* stage; int status; };
void checked(mfxStatus status, const char* stage) { if (status < 0) throw Failure{stage, status}; }
struct Encoder {
    Loader loader;
    Allocator allocator;
    mfxSession session{};
    bool initialized = false;
    bool force_key = true;
    bool intra_refresh = false;
    uint32_t fps{};
    uint64_t sequence = 0;
    mfxVideoParam parameters{};
    mfxExtCodingOption option1{};
    mfxExtCodingOption2 option2{};
    mfxExtCodingOption3 option3{};
    mfxExtBuffer* extensions[3]{};
    Surface input;
    mfxFrameSurface1 surface{};
    explicit Encoder(ID3D11Device* device) : allocator(device) {}
    ~Encoder() {
        if (initialized) MFXVideoENCODE_Close(session);
        if (session) MFXClose(session);
    }
    void bitrate(uint32_t bps) {
        parameters.mfx.TargetKbps = parameters.mfx.MaxKbps = static_cast<mfxU16>((bps + 999) / 1000);
        // oneVPL uses kilobytes; 100 ms of the target bitrate, not kilobits.
        parameters.mfx.BufferSizeInKB = static_cast<mfxU16>(std::max(2u, (bps + 79999) / 80000));
        parameters.mfx.InitialDelayInKB = parameters.mfx.BufferSizeInKB / 2;
    }
    void verify() {
        checked(MFXVideoENCODE_GetVideoParam(session, &parameters), "GetVideoParam");
        std::fprintf(stderr, "oneVPL settings readback AsyncDepth=%u GopRefDist=%u IOPattern=%u rate_control=%u LowDelayBRC=%u\n", parameters.AsyncDepth, parameters.mfx.GopRefDist, parameters.IOPattern, parameters.mfx.RateControlMethod, option3.LowDelayBRC);
        if (parameters.AsyncDepth != 1 || parameters.mfx.GopRefDist != 1 ||
            parameters.IOPattern != MFX_IOPATTERN_IN_VIDEO_MEMORY ||
            parameters.mfx.RateControlMethod != MFX_RATECONTROL_VBR ||
            option3.LowDelayBRC != MFX_CODINGOPTION_ON) throw Failure{"low-latency settings readback", MFX_ERR_UNSUPPORTED};
        if (intra_refresh && (option2.IntRefType != MFX_REFRESH_VERTICAL || option2.IntRefCycleSize != std::max(2u,fps))) throw Failure{"intra refresh settings readback", MFX_ERR_UNSUPPORTED};
        std::fprintf(stderr,"oneVPL intra_refresh_type=%u cycle=%u\n",option2.IntRefType,option2.IntRefCycleSize);
        std::fprintf(stderr, "oneVPL AsyncDepth=%u capped-VBR target_kbps=%u max_kbps=%u buffer_KB=%u initial_KB=%u GopRefDist=%u LowDelayBRC=%u crop=%ux%u surface=%ux%u\n", parameters.AsyncDepth, parameters.mfx.TargetKbps, parameters.mfx.MaxKbps, parameters.mfx.BufferSizeInKB, parameters.mfx.InitialDelayInKB, parameters.mfx.GopRefDist, option3.LowDelayBRC, parameters.mfx.FrameInfo.CropW, parameters.mfx.FrameInfo.CropH, parameters.mfx.FrameInfo.Width, parameters.mfx.FrameInfo.Height);
    }
    int open(ID3D11Device* device, uint32_t width, uint32_t height, uint32_t bps, uint32_t rate, bool refresh, char* error, uint32_t capacity) {
        auto status = open_session(loader.value, device, &session, error, capacity);
        if (status < 0) return status;
        checked(MFXVideoCORE_SetFrameAllocator(session, &allocator.callbacks), "SetFrameAllocator");
        fps = rate;
        intra_refresh = refresh;
        parameters.AsyncDepth = 1;
        parameters.IOPattern = MFX_IOPATTERN_IN_VIDEO_MEMORY;
        parameters.mfx.CodecId = MFX_CODEC_AVC;
        parameters.mfx.CodecProfile = MFX_PROFILE_AVC_BASELINE;
        parameters.mfx.TargetUsage = MFX_TARGETUSAGE_BEST_SPEED;
        // Intel documents LowDelayBRC for VBR/QVBR/VCM, not CBR. Equal target
        // and maximum rates retain the cap while enabling the low-delay mode.
        parameters.mfx.RateControlMethod = MFX_RATECONTROL_VBR;
        parameters.mfx.GopPicSize = static_cast<mfxU16>(fps * 2);
        parameters.mfx.GopRefDist = parameters.mfx.NumRefFrame = 1;
        auto& info = parameters.mfx.FrameInfo;
        info.FourCC = MFX_FOURCC_NV12;
        info.ChromaFormat = MFX_CHROMAFORMAT_YUV420;
        info.PicStruct = MFX_PICSTRUCT_PROGRESSIVE;
        info.Width = static_cast<mfxU16>((width + 15) & ~15u);
        info.Height = static_cast<mfxU16>((height + 15) & ~15u);
        info.CropW = static_cast<mfxU16>((width + 1) & ~1u);
        info.CropH = static_cast<mfxU16>((height + 1) & ~1u);
        info.FrameRateExtN = fps;
        info.FrameRateExtD = 1;
        bitrate(bps);
        option1.Header = {MFX_EXTBUFF_CODING_OPTION, sizeof(option1)};
        option1.NalHrdConformance = option1.VuiNalHrdParameters = MFX_CODINGOPTION_ON;
        option1.MaxDecFrameBuffering = 1;
        option2.Header = {MFX_EXTBUFF_CODING_OPTION2, sizeof(option2)};
        option2.BRefType = MFX_B_REF_OFF;
        option2.RepeatPPS = MFX_CODINGOPTION_ON;
        if (intra_refresh) {
            parameters.mfx.GopPicSize = 0xffff;
            option2.IntRefType = MFX_REFRESH_VERTICAL;
            option2.IntRefCycleSize = static_cast<mfxU16>(std::max(2u,fps));
        }
        option3.Header = {MFX_EXTBUFF_CODING_OPTION3, sizeof(option3)};
        option3.LowDelayBRC = MFX_CODINGOPTION_ON;
        extensions[0] = &option1.Header; extensions[1] = &option2.Header; extensions[2] = &option3.Header;
        parameters.ExtParam = extensions;
        parameters.NumExtParam = 3;
        checked(MFXVideoENCODE_Query(session, &parameters, &parameters), "ENCODE Query");
        checked(MFXVideoENCODE_Init(session, &parameters), "ENCODE Init");
        initialized = true;
        verify();
        surface.Info = parameters.mfx.FrameInfo;
        surface.Data.MemId = &input;
        return 0;
    }
    void reset(uint32_t bps) {
        if (surface.Data.Locked) throw Failure{"input retained before reset", MFX_ERR_LOCK_MEMORY};
        bitrate(bps);
        auto status = MFXVideoENCODE_Reset(session, &parameters);
        if (status < 0) {
            // Some legacy runtimes cannot resize HRD during Reset. Reinitialize
            // the codec in the same hardware session, keeping the D3D device.
            auto closed = MFXVideoENCODE_Close(session);
            initialized = false;
            checked(closed, "ENCODE Close before bitrate restart");
            checked(MFXVideoENCODE_Init(session, &parameters), "ENCODE Init bitrate restart");
            initialized = true;
        }
        verify();
        force_key = true;
    }
    void encode(ID3D11Texture2D* texture, bool key, uint8_t* data, uint32_t capacity, uint32_t* written) {
        if (surface.Data.Locked) throw Failure{"input retained before encode", MFX_ERR_LOCK_MEMORY};
        D3D11_TEXTURE2D_DESC desc{};
        texture->GetDesc(&desc);
        ComPtr<ID3D11Device> source_device;
        texture->GetDevice(&source_device);
        if (desc.Format != DXGI_FORMAT_NV12 || desc.Width != surface.Info.Width || desc.Height != surface.Info.Height ||
            source_device.Get() != allocator.device.Get()) throw Failure{"NV12 input dimensions/device", MFX_ERR_INVALID_VIDEO_PARAM};
        input.texture = texture;
        surface.Data.TimeStamp = sequence * 90000 / fps;
        surface.Data.FrameOrder = static_cast<mfxU32>(sequence++);
        mfxEncodeCtrl control{};
        if (key || force_key) control.FrameType = MFX_FRAMETYPE_I | MFX_FRAMETYPE_IDR | MFX_FRAMETYPE_REF;
        mfxBitstream bitstream{};
        bitstream.Data = data;
        bitstream.MaxLength = capacity;
        mfxSyncPoint sync{};
        auto deadline = GetTickCount64() + 1000;
        mfxStatus status;
        do {
            status = MFXVideoENCODE_EncodeFrameAsync(session, &control, &surface, &bitstream, &sync);
            if (status != MFX_WRN_DEVICE_BUSY) break;
            if (GetTickCount64() >= deadline) throw Failure{"ENCODE busy deadline", MFX_ERR_DEVICE_FAILED};
            Sleep(1);
        } while (true);
        checked(status, "EncodeFrameAsync");
        if (!sync) throw Failure{"encode buffered a frame", MFX_ERR_MORE_DATA};
        status = MFXVideoCORE_SyncOperation(session, sync, 1000);
        if (status != MFX_ERR_NONE) throw Failure{"SyncOperation deadline/status", status};
        if (!bitstream.DataLength || bitstream.DataOffset > capacity || bitstream.DataLength > capacity - bitstream.DataOffset ||
            (bitstream.FrameType & MFX_FRAMETYPE_B) || bitstream.TimeStamp != surface.Data.TimeStamp ||
            (bitstream.DecodeTimeStamp != MFX_TIMESTAMP_UNKNOWN && bitstream.DecodeTimeStamp != bitstream.TimeStamp)) {
            throw Failure{"empty/reordered/delayed bitstream", MFX_ERR_UNDEFINED_BEHAVIOR};
        }
        if (surface.Data.Locked) throw Failure{"input retained after synchronous output", MFX_ERR_LOCK_MEMORY};
        if (bitstream.DataOffset) std::memmove(data, data + bitstream.DataOffset, bitstream.DataLength);
        *written = bitstream.DataLength;
        force_key = false;
        input.texture.Reset();
    }
};
}

extern "C" int prts_vpl_probe(void* device, char* output, uint32_t capacity) {
    Loader loader;
    mfxSession session{};
    int status = open_session(loader.value, device, &session, output, capacity);
    if (status < 0) return status;
    status = MFXClose(session);
    return status < 0 ? message(output, capacity, "MFXClose", status) : 0;
}

extern "C" int prts_vpl_create(void* device, uint32_t width, uint32_t height, uint32_t bps, uint32_t fps, uint32_t intra_refresh, void** output, char* error, uint32_t capacity) {
    if (!device || !output || !width || !height || width > 4096 || height > 4096 || uint64_t(width) * height > 8294400 ||
        !fps || fps > 60 || bps < 1000 || bps > 50000000) return message(error, capacity, "invalid encoder configuration", MFX_ERR_INVALID_VIDEO_PARAM);
    *output = nullptr;
    try {
        auto encoder = std::make_unique<Encoder>(static_cast<ID3D11Device*>(device));
        auto status = encoder->open(static_cast<ID3D11Device*>(device), width, height, bps, fps, intra_refresh != 0, error, capacity);
        if (status < 0) return status;
        *output = encoder.release();
        return 0;
    } catch (const Failure& failure) { return message(error, capacity, failure.stage, failure.status); }
    catch (...) { return message(error, capacity, "encoder allocation/exception", MFX_ERR_MEMORY_ALLOC); }
}
extern "C" void prts_vpl_destroy(void* encoder) { delete static_cast<Encoder*>(encoder); }
extern "C" int prts_vpl_bitrate(void* encoder, uint32_t bps, char* error, uint32_t capacity) {
    if (!encoder || bps < 1000 || bps > 50000000) return message(error, capacity, "invalid bitrate", MFX_ERR_INVALID_VIDEO_PARAM);
    try { static_cast<Encoder*>(encoder)->reset(bps); return 0; }
    catch (const Failure& failure) { return message(error, capacity, failure.stage, failure.status); }
    catch (...) { return message(error, capacity, "bitrate exception", MFX_ERR_MEMORY_ALLOC); }
}
extern "C" int prts_vpl_encode(void* encoder, void* texture, uint32_t key, uint8_t* data, uint32_t size, uint32_t* written, char* error, uint32_t capacity) {
    if (!encoder || !texture || !data || !written || !size) return message(error, capacity, "invalid encode request", MFX_ERR_NULL_PTR);
    *written = 0;
    try {
        static_cast<Encoder*>(encoder)->encode(static_cast<ID3D11Texture2D*>(texture), key != 0, data, size, written);
        return 0;
    } catch (const Failure& failure) { return message(error, capacity, failure.stage, failure.status); }
    catch (...) { return message(error, capacity, "encode exception", MFX_ERR_MEMORY_ALLOC); }
}

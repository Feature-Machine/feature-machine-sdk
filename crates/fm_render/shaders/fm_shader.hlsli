// Feature Machine shader contract v1 (shader pipeline plan, slice 2). Every pass, built-in or from a
// feature package, is one pixel shader with this root signature; the host owns the full-screen
// vertex shader (fullscreen.hlsl). An author includes this file and writes:
//   [RootSignature(FM_RS)] float4 main(FmPixel i) : SV_Target { ... }
// fm_color is the image before this pass, fm_depth the scene depth (0 = sky; black without depth).
// The table is volatile: the host writes the depth descriptor only when a pass first needs it.
#ifndef FM_SHADER_HLSLI
#define FM_SHADER_HLSLI

// 40 root constants: the host block (8) and the feature block (32 floats, fm_params).
#define FM_RS \
    "RootConstants(num32BitConstants=40, b0, visibility=SHADER_VISIBILITY_PIXEL), " \
    "DescriptorTable(SRV(t0, numDescriptors=2, flags=DESCRIPTORS_VOLATILE), visibility=SHADER_VISIBILITY_PIXEL), " \
    "StaticSampler(s0, filter=FILTER_MIN_MAG_MIP_LINEAR, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP, visibility=SHADER_VISIBILITY_PIXEL), " \
    "StaticSampler(s1, filter=FILTER_MIN_MAG_MIP_POINT, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP, visibility=SHADER_VISIBILITY_PIXEL)"

// Host block: seconds since the pass started (wraps each hour), frame count, target size in
// pixels, camera near plane (m) and tan(vertical FOV / 2). Then the feature's 32 parameters.
cbuffer FmFrame : register(b0) {
    float fm_time; uint fm_frame; float2 fm_size;
    float fm_near; float fm_tan_half_v; float2 fm_reserved;
    float4 fm_params[8];
};
Texture2D<float4> fm_color : register(t0);
Texture2D<float> fm_depth : register(t1);
SamplerState fm_linear : register(s0);
SamplerState fm_point : register(s1);

// The vertex output: pixel position and uv (0,0 top left to 1,1 bottom right).
struct FmPixel { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

#endif

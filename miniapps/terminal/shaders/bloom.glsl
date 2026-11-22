// Bloom: bright text glows. Ghostty's custom-shader format (mainImage, iChannel0 = the
// terminal screen). Static (no iTime).
//
// Around each pixel, samples on a golden-angle spiral (even coverage with few samples),
// weighted by closeness; only what is brighter than THRESHOLD adds light.

// Pixels between the spiral's rings
#define SPREAD 1.6
// Luminance where the glow starts
#define THRESHOLD 0.2
// How much light the glow adds
#define STRENGTH 0.12
#define SAMPLES 24

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    vec3 base = texture(iChannel0, uv).rgb;
    vec3 glow = vec3(0.0);
    for (int i = 0; i < SAMPLES; i++) {
        float r = sqrt(float(i) + 0.5);
        float a = float(i) * 2.39996323; // golden angle, in radians
        vec2 offset = vec2(cos(a), sin(a)) * r * SPREAD / iResolution.xy;
        vec3 c = texture(iChannel0, uv + offset).rgb;
        float l = dot(c, vec3(0.299, 0.587, 0.114));
        glow += c * max(l - THRESHOLD, 0.0) / r;
    }
    fragColor = vec4(clamp(base + glow * STRENGTH, 0.0, 1.0), 1.0);
}

// Glitch on error: a short shake when the shell rings the bell or a command fails (the
// latter with shells that mark their prompt, OSC 133). Colors split and a few slices of
// the screen slide sideways, then it settles. Ghostty's custom-shader format (mainImage,
// iChannel0 = the terminal screen) plus MiniUtils' iTimeError; it draws only after an error.
//
// Each number #define is an adjustment in the Library; the [min, max] above it is its
// range.
#pragma miniutils events

// How long it lasts (s)
// [0.05, 1.5]
#define DURATION 0.35
// How strong
// [0, 1]
#define INTENSITY 0.7
// Color split (px)
// [0, 20]
#define SPLIT 6.0
// Slice slide (px)
// [0, 80]
#define SHIFT 24.0
// Slices across the screen
// [4, 40]
#define SLICES 14.0

float hash(float n) {
    return fract(sin(n) * 43758.5453);
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    float age = iTime - iTimeError;
    if (age < 0.0 || age > DURATION) {
        fragColor = texture(iChannel0, uv);
        return;
    }
    float k = 1.0 - age / DURATION;
    k = k * k * INTENSITY;
    // a new pattern 40 times a second
    float tick = floor(iTime * 40.0);
    float row = floor(uv.y * SLICES);
    float slide = hash(row * 13.7 + tick) > 0.65 ? (hash(row + tick * 3.1) - 0.5) * 2.0 * SHIFT * k : 0.0;
    vec2 px = 1.0 / iResolution.xy;
    vec2 p = uv + vec2(slide, 0.0) * px;
    float s = SPLIT * k;
    vec4 c = texture(iChannel0, p);
    float red = texture(iChannel0, p + vec2(s, 0.0) * px).r;
    float blue = texture(iChannel0, p - vec2(s, 0.0) * px).b;
    fragColor = vec4(red, c.g, blue, c.a);
}

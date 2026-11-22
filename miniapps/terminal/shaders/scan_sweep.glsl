// Scan sweep: every few seconds a light band runs down the screen, like a radar.
// Ghostty's custom-shader format (mainImage, iChannel0 = the terminal screen). Animated:
// the window draws on every frame while it shows.
//
// Each number #define is an adjustment in the Library; the [min, max] above it is its
// range.

// Seconds between sweeps
// [1, 30]
#define PERIOD 6.0
// How long a sweep takes (s)
// [0.3, 5]
#define SWEEP 1.6
// Band height (px)
// [4, 200]
#define BAND 60.0
// How bright
// [0, 1]
#define STRENGTH 0.18

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec4 c = texture(iChannel0, fragCoord / iResolution.xy);
    float t = mod(iTime, PERIOD);
    if (t > SWEEP) {
        fragColor = c;
        return;
    }
    // the leading edge goes from the top down (GL's y grows upward); the band trails
    // above it
    float edge = iResolution.y * (1.0 - t / SWEEP);
    float above = fragCoord.y - edge;
    float band = above >= 0.0 && above < BAND ? 1.0 - above / BAND : 0.0;
    float k = band * band * STRENGTH;
    // light on a dark theme; on a light one (where light would not show) a tint
    bool light = dot(iBackgroundColor, vec3(0.299, 0.587, 0.114)) > 0.5;
    vec3 color = light ? mix(c.rgb, iCursorColor, k) : c.rgb + iCursorColor * k;
    fragColor = vec4(clamp(color, 0.0, 1.0), c.a);
}

// Mood vignette: the corners glow softly in the color of the window mascot's mood
// (calm, busy, alarm, asleep…), breathing slowly. Ghostty's custom-shader format
// (mainImage, iChannel0 = the terminal screen) plus MiniUtils' iMoodColor. Animated: the
// window draws on every frame while it shows.
//
// Each number #define is an adjustment in the Library; the [min, max] above it is its
// range.

// How far in it reaches
// [0.1, 1]
#define SIZE 0.55
// How strong
// [0, 1]
#define STRENGTH 0.35
// Seconds per breath
// [1, 12]
#define PERIOD 4.0
// How much it breathes
// [0, 1]
#define PULSE 0.5

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    vec4 c = texture(iChannel0, uv);
    // 0 in the middle, 1 in the corners
    float r = length(uv * 2.0 - 1.0) / 1.41421356;
    float edge = smoothstep(1.0 - SIZE, 1.0, r);
    float breath = 1.0 - PULSE * 0.5 * (1.0 + cos(6.2831853 * iTime / PERIOD));
    fragColor = vec4(clamp(c.rgb + iMoodColor * edge * STRENGTH * breath, 0.0, 1.0), c.a);
}

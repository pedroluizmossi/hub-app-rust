// Phosphor: what leaves the screen fades slowly, like the glow of an old monitor's tube.
// Ghostty's custom-shader format (mainImage, iChannel0 = the terminal screen) plus
// MiniUtils' iChannel1 (what this shader drew the frame before); it draws only while
// something changes.
//
// Each pixel keeps what stands out more from the background: what is there now or what was
// there, faded toward the background by the time gone by (so it works on light themes
// too). Each number #define is an adjustment in the Library; the [min, max] above it is
// its range.
#pragma miniutils events

// How long a trace lingers (s)
// [0.05, 2]
#define PERSIST 0.35
// Trace color: own (0) to the cursor's (1)
// [0, 1]
#define TINT 0.5

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    vec4 now = texture(iChannel0, uv);
    vec3 bg = iBackgroundColor;
    vec3 trace = mix(bg, texture(iChannel1, uv).rgb, exp(-max(iTimeDelta, 0.0) / PERSIST));
    float strength = length(trace - bg);
    // toward the cursor's color, as far from the background as the trace is
    vec3 tinted = mix(bg, iCursorColor, clamp(strength / max(length(iCursorColor - bg), 0.05), 0.0, 1.0));
    trace = mix(trace, tinted, TINT);
    // only where the old trace stands out more than what is there now: what is still on
    // screen keeps its own color
    float gone = smoothstep(0.0, 0.06, strength - length(now.rgb - bg));
    fragColor = vec4(mix(now.rgb, trace, gone), now.a);
}

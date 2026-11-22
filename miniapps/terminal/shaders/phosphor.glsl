// Phosphor: what leaves the screen fades slowly, like the glow of an old monitor's tube.
// Ghostty's custom-shader format (mainImage, iChannel0 = the terminal screen) plus
// MiniUtils' iChannel1 (what this shader drew the frame before); it draws only while
// something changes.
//
// Each pixel keeps the brighter of what is there now and what was there, dimmed by the
// time gone by. Each number #define is an adjustment in the Library; the [min, max] above
// it is its range.
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
    vec3 luma = vec3(0.299, 0.587, 0.114);
    vec3 trace = texture(iChannel1, uv).rgb * exp(-max(iTimeDelta, 0.0) / PERSIST);
    float lt = dot(trace, luma);
    trace = mix(trace, iCursorColor * lt, TINT);
    // only where the old light is brighter than what is there now: what is still on
    // screen keeps its own color
    float gone = smoothstep(0.0, 0.06, lt - dot(now.rgb, luma));
    fragColor = vec4(mix(now.rgb, trace, gone), now.a);
}

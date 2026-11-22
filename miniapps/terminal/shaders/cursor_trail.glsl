// Cursor trail: when the cursor jumps, a streak runs from where it was to where it is and
// fades. Moves of a cell or two (typing) leave none. Ghostty's custom-shader format
// (mainImage, iChannel0 = the terminal screen); it draws only after a move.
//
// The streak is a capsule from a tail that catches up with the cursor, thinner at the
// tail. Each number #define is an adjustment in the Library; the [min, max] above it is
// its range.
#pragma miniutils events

// How long the streak lasts (s)
// [0.05, 1]
#define DURATION 0.22
// Shorter moves leave no streak (cursor widths)
// [0, 10]
#define MIN_JUMP 2.5
// Streak width (part of the cursor's height)
// [0.1, 1]
#define WIDTH 0.6
// How strongly it shows
// [0, 1]
#define OPACITY 0.7

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    vec4 base = texture(iChannel0, uv);
    fragColor = base;
    float age = iTime - iTimeCursorChange;
    if (age < 0.0 || age > DURATION) {
        return;
    }
    // centers: x and the top edge's y, from the bottom left
    vec2 now = iCurrentCursor.xy + vec2(iCurrentCursor.z, -iCurrentCursor.w) * 0.5;
    vec2 before = iPreviousCursor.xy + vec2(iPreviousCursor.z, -iPreviousCursor.w) * 0.5;
    if (distance(now, before) < MIN_JUMP * max(iCurrentCursor.z, 1.0)) {
        return;
    }
    float t = age / DURATION;
    float ease = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
    vec2 tail = mix(before, now, ease);
    vec2 pa = fragCoord - tail;
    vec2 ba = now - tail;
    float h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.0001), 0.0, 1.0);
    float d = length(pa - ba * h);
    float radius = iCurrentCursor.w * WIDTH * 0.5 * mix(0.35, 1.0, h);
    float inside = 1.0 - smoothstep(radius * 0.6, radius, d);
    float alpha = inside * (1.0 - t) * OPACITY * mix(0.3, 1.0, h);
    fragColor = vec4(mix(base.rgb, iCurrentCursorColor.rgb, alpha), base.a);
}

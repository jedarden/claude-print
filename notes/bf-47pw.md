# Verification Report: PROMPT_INJECTED Transition Detection

## Task: bf-47pw

Verify that the phase change detection for PromptInjected works correctly in the event loop callback.

---

## Code Analysis

### 1. Phase Change Detection Logic (session.rs:358-360)

```rust
let current_phase = startup.phase();
if last_phase != *current_phase && current_phase.is_prompt_injected() {
    watchdog_state_clone.mark_prompt_injected();
    // ... spawn stream-json reader ...
}
```

**Status: ✅ VERIFIED**

The logic correctly detects a transition TO PromptInjected:
- `last_phase != *current_phase` — detects ANY phase change
- `current_phase.is_prompt_injected()` — filters for transitions TO PromptInjected only

This condition ensures the detection fires ONLY when transitioning TO PromptInjected, not when:
- The phase stays the same (e.g., remaining in PromptInjected)
- Transitioning to another phase (e.g., PromptInjected → something else, though currently not possible)

### 2. last_phase Update (session.rs:377)

```rust
last_phase = current_phase.clone();
```

**Status: ✅ VERIFIED**

The update happens AFTER the phase change detection, which is the correct ordering:
1. Check for transition using old `last_phase` value
2. Execute transition actions if needed
3. Update `last_phase` to current value for next iteration

This prevents the detection from firing twice for the same transition.

### 3. is_prompt_injected() Method (startup.rs:49-54)

```rust
impl StartupPhase {
    pub fn is_prompt_injected(&self) -> bool {
        matches!(self, Self::PromptInjected)
    }
}
```

**Status: ✅ VERIFIED**

The method exists and works correctly. It returns `true` only when the phase is `StartupPhase::PromptInjected`.

### 4. One-Time Firing Behavior

**Status: ✅ VERIFIED**

The detection fires exactly once when transitioning TO PromptInjected:

**State progression:**

| Iteration | last_phase | current_phase | Condition Result | Action |
|-----------|------------|---------------|------------------|--------|
| 1 (before) | TrustDismissed | TrustDismissed | `!=` false | No action |
| 2 (transition) | TrustDismissed | PromptInjected | `!=` true + `is_prompt_injected()` true | **Action fires** |
| 3 (after) | PromptInjected | PromptInjected | `!=` false | No action |

After the transition fires, `last_phase` is updated to `PromptInjected`. On all subsequent iterations, `last_phase == current_phase`, so the condition is false and no action is taken.

---

## Supporting Evidence

### StartupPhase Definition (startup.rs:38-47)

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum StartupPhase {
    Waiting,
    TrustDismissed,
    PromptInjected,
}
```

The `PartialEq` derive ensures `==` and `!=` operators work correctly for comparing phases.

### Related Test Coverage

The existing test suite covers the idle-gap timing behavior:

- `idle_gap_fires_after_silence` — verifies transition from TrustDismissed → PromptInjected
- `idle_gap_does_not_fire_after_prompt_injected` — verifies that once in PromptInjected, no further actions occur
- `idle_gap_resets_on_new_output` — verifies the idle-gap timing logic

---

## Conclusion

**All acceptance criteria met:**

1. ✅ Phase change detection logic at line 359-360 is correct
2. ✅ `last_phase` is updated correctly at line 377
3. ✅ `is_prompt_injected()` method exists and works correctly
4. ✅ The detection fires only once when transitioning TO PromptInjected

The PROMPT_INJECTED transition detection mechanism is implemented correctly and will:
- Detect the transition from TrustDismissed to PromptInjected exactly once
- Mark the prompt injection timestamp in the watchdog state
- Spawn the stream-json reader (if using stream-json output format)
- Never fire again for the same transition

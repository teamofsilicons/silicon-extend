// No SPDX header upstream. LibADB Android's COPYING puts every contribution under
// GPL-3.0-or-later OR Apache-2.0; Silicon Extend uses it under Apache-2.0.

package io.github.muntashirakon.adb;

public class AdbPairingRequiredException extends Exception {
    public AdbPairingRequiredException(String message) {
        super(message);
    }
}

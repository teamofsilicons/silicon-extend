/** Fixture identities and secrets for the mock Extend service. Shared with the Playwright tests. */

export const MOCK_PORT = Number(process.env.MOCK_PORT || 8490);

/** Test app secrets must match ^ask_[A-Za-z0-9_-]{43}$. */
export const TEST_SECRET = "ask_" + "checkoutE2Etestenvironment".padEnd(43, "0");
export const TEST_ENVIRONMENT_ID = "9b3e0c1a-2f4d-4e6b-8a7c-1d2e3f4a5b6c";
export const TEST_ENVIRONMENT_NAME = "checkout-e2e";
/** Well-formed but unknown: the mock answers 401 testing_secret_invalid. */
export const UNKNOWN_SECRET = "ask_" + "revoked".padEnd(43, "x");

/**
 * Production SLTs the mock accepts any number of times, for convenience: `oac_<handle>` signs in
 * as `c:<handle>`, `oac_si_<handle>` as `si:<handle>`. The mock consent screen mints real
 * single-use ones.
 */
export const SLT_SAKET = "oac_saket";
export const SLT_ALICE = "oac_alice";
export const SLT_CHEF = "oac_si_chef";

export const TEAM = "acme";
export const OTHER_TEAM = "labs";

export const DEVICE_PIXEL = "7c1e09ab";
export const DEVICE_MAC = "2e7f00d1";
export const DEVICE_TV = "0d44e1f2";
export const DEVICE_IPHONE = "51ab93c0";

/** A live pairing code seeded at start (and after every reset), shown by an "Android" app. */
export const SEEDED_CODE = "4F9C2A";

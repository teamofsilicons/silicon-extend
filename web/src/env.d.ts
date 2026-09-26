/// <reference types="vite/client" />
interface ImportMetaEnv {
  /** Extend API base URL. `same-origin` means relative paths (dev proxy or a Vercel rewrite). */
  readonly VITE_EXTEND_API_URL?: string;
  /** Where the Silicon IAM consent screen lives, when it can't be derived from `iam_base_url`. */
  readonly VITE_IAM_LOGIN_URL?: string;
}
interface ImportMeta {
  readonly env: ImportMetaEnv;
}

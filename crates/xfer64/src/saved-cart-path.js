/**
 * Shared SD path normalization and “saved folder” probing for main explorer + quick upload.
 * Matches persisted `quick_upload_cart_path` in settings.
 */

/**
 * @param {unknown} p
 * @returns {string}
 */
export function normalizeUsbPath(p) {
  let s = String(p || "").trim().replace(/\\/g, "/");
  s = s.replace(/^\/+/, "");
  s = s.replace(/\/+$/, "");
  return s;
}

/**
 * Returns `normalized` if the cart lists that folder; otherwise clears the stale setting and returns "".
 * @param {function(string, object=): Promise<unknown>} invoke
 * @param {string} normalized
 * @returns {Promise<string>}
 */
export async function probeSavedCartFolderReachable(invoke, normalized) {
  if (!normalized) return "";
  try {
    await invoke("cart_serial_list_dir_page", {
      path: normalized,
      offset: 0,
      limit: 1,
      fresh: true,
    });
    return normalized;
  } catch {
    try {
      await invoke("explorer_set_quick_upload_cart_path", { path: "" });
    } catch {
      /* ignore */
    }
    return "";
  }
}

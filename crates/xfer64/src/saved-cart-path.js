/**
 * Shared by the main explorer and Quick upload: SD path normalization, “saved folder” probing, and
 * pausing the Multi64 bridge around cart access.
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

/** Same key and default as the main explorer's `daemonListenUrl`. */
const LS_DAEMON_LISTEN = "multi64.explorer.daemonListen";
const DEFAULT_DAEMON_LISTEN = "http://127.0.0.1:38765";

/** multi64d's HTTP address. */
export function bridgeListenUrl() {
  try {
    return localStorage.getItem(LS_DAEMON_LISTEN) || DEFAULT_DAEMON_LISTEN;
  } catch {
    return DEFAULT_DAEMON_LISTEN;
  }
}

/**
 * Run `fn` with the Multi64 bridge paused when multi64d is running, then resume it: multi64d holds
 * the cart's serial port while the bridge runs, so any cart access has to go through here.
 *
 * A resume is attempted after every release attempt, including one that failed or timed out, since
 * multi64d may still apply that release late; `fn` never runs without a release.
 *
 * @template T
 * @param {function(string, object=): Promise<unknown>} invoke
 * @param {string} listen
 * @param {() => Promise<T>} fn
 * @param {{ onResumeError?: (e: unknown) => void }} [opts] told when the resume after `fn` fails
 * @returns {Promise<T>}
 */
export async function withBridgePaused(invoke, listen, fn, opts = {}) {
  let up = false;
  try {
    up = (await invoke("explorer_daemon_probe", { listen }))?.up === true;
  } catch {
    up = false;
  }
  if (!up) return fn();
  try {
    await invoke("explorer_daemon_release", { listen });
  } catch (releaseError) {
    await invoke("explorer_daemon_resume", { listen }).catch(() => {});
    throw releaseError;
  }
  try {
    return await fn();
  } finally {
    try {
      await invoke("explorer_daemon_resume", { listen });
    } catch (e) {
      opts.onResumeError?.(e);
    }
  }
}

/**
 * Check the saved Quick upload folder on the cart.
 *
 * - `ok`: the cart lists it.
 * - `missing`: the cart lists but that folder doesn't; the stale setting is cleared.
 * - `unknown`: the cart couldn't be read at all (unplugged, or its port busy); the setting is kept.
 *   Clearing it here used to send the next upload to the cart root.
 *
 * @param {function(string, object=): Promise<unknown>} invoke
 * @param {string} normalized
 * @param {{ withBridge?: <T>(fn: () => Promise<T>) => Promise<T> }} [opts] wraps the cart reads,
 *   normally in a bridge pause. If it throws, the result is `unknown`.
 * @returns {Promise<{ status: "none" | "ok" | "missing" | "unknown", path: string }>} `path` is
 *   the folder to start in: `normalized`, or "" once it is known to be missing.
 */
export async function probeSavedCartFolderReachable(invoke, normalized, opts = {}) {
  if (!normalized) return { status: "none", path: "" };
  const withBridge = opts.withBridge || ((fn) => fn());
  const lists = async (path) => {
    try {
      await invoke("cart_serial_list_dir_page", { path, offset: 0, limit: 1, fresh: true });
      return true;
    } catch {
      return false;
    }
  };
  let status;
  try {
    status = await withBridge(async () => {
      if (await lists(normalized)) return "ok";
      return (await lists("")) ? "missing" : "unknown";
    });
  } catch {
    status = "unknown";
  }
  if (status === "missing") {
    try {
      await invoke("explorer_set_quick_upload_cart_path", { path: "" });
    } catch {
      /* ignore */
    }
    return { status, path: "" };
  }
  return { status, path: normalized };
}

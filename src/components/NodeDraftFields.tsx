import { GlassSwitchControl } from "./GlassSwitchControl";
import { SolidSelect } from "./SolidSelect";
import type { ManualNodeDraft } from "../types";

export const NODE_PROTOCOLS = [
  { value: "vless", label: "VLESS" },
  { value: "vmess", label: "VMess" },
  { value: "trojan", label: "Trojan" },
  { value: "shadowsocks", label: "Shadowsocks" },
  { value: "hysteria2", label: "Hysteria2" },
  { value: "tuic", label: "TUIC" },
  { value: "socks5", label: "SOCKS5" },
  { value: "http", label: "HTTP" },
  { value: "anytls", label: "AnyTLS" },
  { value: "snell", label: "Snell" },
  { value: "hysteria", label: "Hysteria" },
  { value: "ssh", label: "SSH" },
  { value: "wireguard", label: "WireGuard" },
  { value: "shadowtls", label: "ShadowTLS" },
  { value: "naive", label: "Naive" },
  { value: "tor", label: "Tor" },
] as const;

const SS_METHODS = [
  "aes-128-gcm",
  "aes-256-gcm",
  "chacha20-ietf-poly1305",
  "2022-blake3-aes-128-gcm",
  "2022-blake3-aes-256-gcm",
  "2022-blake3-chacha20-poly1305",
  "none",
];

const VMESS_SECURITY = ["auto", "aes-128-gcm", "chacha20-poly1305", "none"];
const NETWORKS = [
  { value: "tcp", label: "TCP" },
  { value: "ws", label: "WebSocket" },
  { value: "grpc", label: "gRPC" },
  { value: "http", label: "HTTP" },
  { value: "httpupgrade", label: "HTTPUpgrade" },
  { value: "xhttp", label: "XHTTP (仅 Xray)" },
];
const FINGERPRINTS = [
  "chrome",
  "firefox",
  "safari",
  "ios",
  "android",
  "edge",
  "random",
];

export function emptyNodeDraft(): ManualNodeDraft {
  return {
    protocol: "vless",
    server: "",
    port: 443,
    tls: true,
    network: "tcp",
    packetEncoding: "xudp",
    method: "aes-256-gcm",
    security: "auto",
    version: 4,
  };
}

function hasTls(protocol: string) {
  return [
    "vless",
    "vmess",
    "trojan",
    "hysteria2",
    "tuic",
    "http",
    "hysteria",
    "anytls",
    "shadowtls",
    "naive",
    "socks5",
  ].includes(protocol);
}

function hasTransport(protocol: string) {
  return ["vless", "vmess", "trojan", "shadowsocks"].includes(protocol);
}

function needsServer(protocol: string) {
  return protocol !== "tor";
}

export function nodeDraftReady(draft: ManualNodeDraft): boolean {
  const p = draft.protocol;
  if (needsServer(p) && (!draft.server.trim() || !draft.port)) return false;
  switch (p) {
    case "shadowsocks":
    case "trojan":
    case "hysteria2":
    case "anytls":
      return !!(draft.password ?? "").trim();
    case "vmess":
    case "vless":
    case "tuic":
      return !!(draft.uuid ?? "").trim();
    case "hysteria":
      return !!(draft.password ?? "").trim();
    case "naive":
      return !!(draft.username ?? "").trim() && !!(draft.password ?? "").trim();
    case "ssh":
      return !!(draft.password ?? "").trim() || !!(draft.privateKey ?? "").trim();
    case "tor":
      return !!(draft.executablePath ?? "").trim();
    case "wireguard":
      return (
        !!(draft.privateKey ?? "").trim() &&
        !!(draft.peerPublicKey ?? "").trim() &&
        !!(draft.localAddress ?? "").trim()
      );
    case "snell":
      return !!(draft.psk ?? draft.password ?? "").trim();
    default:
      return true;
  }
}

interface Props {
  value: ManualNodeDraft;
  disabled?: boolean;
  onChange: (next: ManualNodeDraft) => void;
}

export function NodeDraftFields({ value, disabled, onChange }: Props) {
  const p = value.protocol || "vless";
  const set = (patch: Partial<ManualNodeDraft>) =>
    onChange({ ...value, ...patch });

  return (
    <div className="node-draft">
      <label className="field">
        <span>协议</span>
        <SolidSelect
          aria-label="协议"
          value={p}
          disabled={disabled}
          options={NODE_PROTOCOLS.map((o) => ({ ...o }))}
          onChange={(protocol) => {
            const tlsOn = [
              "vless",
              "trojan",
              "hysteria2",
              "tuic",
              "anytls",
              "hysteria",
              "shadowtls",
              "naive",
            ].includes(protocol);
            set({
              protocol,
              tls: tlsOn,
              port:
                value.port ||
                (protocol === "shadowsocks"
                  ? 8388
                  : protocol === "socks5"
                    ? 1080
                    : 443),
            });
          }}
        />
      </label>

      {needsServer(p) && (
        <div className="field-grid">
          <label className="field">
            <span>服务器</span>
            <input
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              value={value.server}
              onChange={(e) => set({ server: e.target.value })}
              placeholder="example.com"
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>端口</span>
            <input
              type="number"
              min={1}
              max={65535}
              value={value.port || ""}
              onChange={(e) =>
                set({ port: Number.parseInt(e.target.value, 10) || 0 })
              }
              disabled={disabled}
            />
          </label>
        </div>
      )}

      {p === "shadowsocks" && (
        <>
          <label className="field">
            <span>加密</span>
            <SolidSelect
              aria-label="加密"
              value={value.method || "aes-256-gcm"}
              disabled={disabled}
              options={SS_METHODS.map((m) => ({ value: m, label: m }))}
              onChange={(method) => set({ method })}
            />
          </label>
          <label className="field">
            <span>密码</span>
            <input
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              value={value.password ?? ""}
              onChange={(e) => set({ password: e.target.value })}
              disabled={disabled}
            />
          </label>
          <div className="field-grid">
            <label className="field">
              <span>插件</span>
              <input
                value={value.plugin ?? ""}
                onChange={(e) => set({ plugin: e.target.value })}
                placeholder="可选"
                disabled={disabled}
              />
            </label>
            <label className="field">
              <span>插件参数</span>
              <input
                value={value.pluginOpts ?? ""}
                onChange={(e) => set({ pluginOpts: e.target.value })}
                placeholder="可选"
                disabled={disabled}
              />
            </label>
          </div>
        </>
      )}

      {(p === "vless" || p === "vmess" || p === "tuic") && (
        <label className="field">
          <span>UUID</span>
          <input
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={value.uuid ?? ""}
            onChange={(e) => set({ uuid: e.target.value })}
            disabled={disabled}
          />
        </label>
      )}

      {p === "vless" && (
        <label className="field">
          <span>Flow</span>
          <input
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={value.flow ?? ""}
            onChange={(e) => set({ flow: e.target.value })}
            placeholder="如 xtls-rprx-vision，可留空"
            disabled={disabled}
          />
        </label>
      )}

      {p === "vmess" && (
        <div className="field-grid">
          <label className="field">
            <span>加密</span>
            <SolidSelect
              aria-label="VMess 加密"
              value={value.security || "auto"}
              disabled={disabled}
              options={VMESS_SECURITY.map((m) => ({ value: m, label: m }))}
              onChange={(security) => set({ security })}
            />
          </label>
          <label className="field">
            <span>alterId</span>
            <input
              type="number"
              min={0}
              value={value.alterId ?? 0}
              onChange={(e) =>
                set({ alterId: Number.parseInt(e.target.value, 10) || 0 })
              }
              disabled={disabled}
            />
          </label>
        </div>
      )}

      {(p === "trojan" ||
        p === "hysteria2" ||
        p === "anytls" ||
        p === "hysteria" ||
        p === "tuic") && (
        <label className="field">
          <span>{p === "hysteria" ? "Auth" : "密码"}</span>
          <input
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={value.password ?? ""}
            onChange={(e) => set({ password: e.target.value })}
            disabled={disabled}
          />
        </label>
      )}

      {(p === "socks5" || p === "http" || p === "naive") && (
        <div className="field-grid">
          <label className="field">
            <span>用户名</span>
            <input
              value={value.username ?? ""}
              onChange={(e) => set({ username: e.target.value })}
              placeholder={p === "naive" ? "必填" : "可选"}
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>密码</span>
            <input
              value={value.password ?? ""}
              onChange={(e) => set({ password: e.target.value })}
              placeholder={p === "naive" ? "必填" : "可选"}
              disabled={disabled}
            />
          </label>
        </div>
      )}

      {(p === "hysteria2" || p === "hysteria") && (
        <div className="field-grid">
          <label className="field">
            <span>上行 Mbps</span>
            <input
              type="number"
              min={0}
              value={value.upMbps ?? ""}
              onChange={(e) =>
                set({
                  upMbps: e.target.value
                    ? Number.parseInt(e.target.value, 10)
                    : null,
                })
              }
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>下行 Mbps</span>
            <input
              type="number"
              min={0}
              value={value.downMbps ?? ""}
              onChange={(e) =>
                set({
                  downMbps: e.target.value
                    ? Number.parseInt(e.target.value, 10)
                    : null,
                })
              }
              disabled={disabled}
            />
          </label>
        </div>
      )}

      {p === "hysteria2" && (
        <div className="field-grid">
          <label className="field">
            <span>混淆</span>
            <input
              value={value.obfs ?? ""}
              onChange={(e) => set({ obfs: e.target.value })}
              placeholder="salamander / 可留空"
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>混淆密码</span>
            <input
              value={value.obfsPassword ?? ""}
              onChange={(e) => set({ obfsPassword: e.target.value })}
              disabled={disabled}
            />
          </label>
        </div>
      )}

      {p === "tuic" && (
        <div className="field-grid">
          <label className="field">
            <span>拥塞控制</span>
            <input
              value={value.congestionControl ?? ""}
              onChange={(e) => set({ congestionControl: e.target.value })}
              placeholder="bbr / cubic"
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>UDP 模式</span>
            <input
              value={value.udpRelayMode ?? ""}
              onChange={(e) => set({ udpRelayMode: e.target.value })}
              placeholder="native / quic"
              disabled={disabled}
            />
          </label>
        </div>
      )}

      {p === "snell" && (
        <>
          <label className="field">
            <span>PSK</span>
            <input
              value={value.psk ?? value.password ?? ""}
              onChange={(e) => set({ psk: e.target.value, password: e.target.value })}
              disabled={disabled}
            />
          </label>
          <div className="field-grid">
            <label className="field">
              <span>版本</span>
              <SolidSelect
                aria-label="Snell 版本"
                value={String(value.version ?? 4)}
                disabled={disabled}
                options={[
                  { value: "4", label: "4" },
                  { value: "6", label: "6" },
                ]}
                onChange={(v) => set({ version: Number(v) })}
              />
            </label>
            <label className="field">
              <span>Obfs</span>
              <input
                value={value.obfs ?? ""}
                onChange={(e) => set({ obfs: e.target.value })}
                placeholder="http / none"
                disabled={disabled}
              />
            </label>
          </div>
        </>
      )}

      {p === "ssh" && (
        <>
          <label className="field">
            <span>用户</span>
            <input
              value={value.user ?? value.username ?? ""}
              onChange={(e) => set({ user: e.target.value })}
              placeholder="root"
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>密码</span>
            <input
              value={value.password ?? ""}
              onChange={(e) => set({ password: e.target.value })}
              placeholder="与私钥二选一"
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>私钥</span>
            <textarea
              className="config-paste"
              value={value.privateKey ?? ""}
              onChange={(e) => set({ privateKey: e.target.value })}
              placeholder="PEM / OpenSSH，可留空"
              disabled={disabled}
              rows={3}
            />
          </label>
        </>
      )}

      {p === "wireguard" && (
        <>
          <label className="field">
            <span>本地地址</span>
            <input
              value={value.localAddress ?? ""}
              onChange={(e) => set({ localAddress: e.target.value })}
              placeholder="10.0.0.2/32"
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>私钥</span>
            <input
              value={value.privateKey ?? ""}
              onChange={(e) => set({ privateKey: e.target.value })}
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span>对端公钥</span>
            <input
              value={value.peerPublicKey ?? ""}
              onChange={(e) => set({ peerPublicKey: e.target.value })}
              disabled={disabled}
            />
          </label>
        </>
      )}

      {p === "shadowtls" && (
        <div className="field-grid">
          <label className="field">
            <span>版本</span>
            <SolidSelect
              aria-label="ShadowTLS 版本"
              value={String(value.version ?? 3)}
              disabled={disabled}
              options={[
                { value: "1", label: "1" },
                { value: "2", label: "2" },
                { value: "3", label: "3" },
              ]}
              onChange={(v) => set({ version: Number(v) })}
            />
          </label>
          <label className="field">
            <span>密码</span>
            <input
              value={value.password ?? ""}
              onChange={(e) => set({ password: e.target.value })}
              disabled={disabled}
            />
          </label>
        </div>
      )}

      {p === "tor" && (
        <label className="field">
          <span>可执行文件</span>
          <input
            value={value.executablePath ?? ""}
            onChange={(e) => set({ executablePath: e.target.value })}
            placeholder="tor 路径"
            disabled={disabled}
          />
        </label>
      )}

      {hasTls(p) && (
        <>
          <div className="via-proxy-row">
            <div>
              <div className="sys-proxy-title">TLS</div>
              <div className="sys-proxy-desc">启用传输层加密</div>
            </div>
            <GlassSwitchControl
              checked={!!value.tls}
              title="TLS"
              disabled={disabled}
              onChange={(tls) => set({ tls })}
            />
          </div>
          {value.tls && (
            <>
              <div className="field-grid">
                <label className="field">
                  <span>SNI</span>
                  <input
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    value={value.sni ?? ""}
                    onChange={(e) => set({ sni: e.target.value })}
                    placeholder="可留空"
                    disabled={disabled}
                  />
                </label>
                <label className="field">
                  <span>指纹</span>
                  <SolidSelect
                    aria-label="uTLS 指纹"
                    value={value.fingerprint || ""}
                    disabled={disabled}
                    placeholder="默认"
                    options={[
                      { value: "", label: "默认" },
                      ...FINGERPRINTS.map((f) => ({ value: f, label: f })),
                    ]}
                    onChange={(fingerprint) =>
                      set({ fingerprint: fingerprint || null })
                    }
                  />
                </label>
              </div>
              <div className="via-proxy-row">
                <div>
                  <div className="sys-proxy-title">跳过证书验证</div>
                </div>
                <GlassSwitchControl
                  checked={!!value.insecure}
                  title="insecure"
                  disabled={disabled}
                  onChange={(insecure) => set({ insecure })}
                />
              </div>
              {(p === "vless" || p === "vmess") && (
                <div className="field-grid">
                  <label className="field">
                    <span>Reality 公钥</span>
                    <input
                      value={value.realityPublicKey ?? ""}
                      onChange={(e) => set({ realityPublicKey: e.target.value })}
                      placeholder="可选"
                      disabled={disabled}
                    />
                  </label>
                  <label className="field">
                    <span>Short ID</span>
                    <input
                      value={value.realityShortId ?? ""}
                      onChange={(e) => set({ realityShortId: e.target.value })}
                      placeholder="可选"
                      disabled={disabled}
                    />
                  </label>
                </div>
              )}
            </>
          )}
        </>
      )}

      {hasTransport(p) && (
        <>
          <label className="field">
            <span>传输</span>
            <SolidSelect
              aria-label="传输"
              value={value.network || "tcp"}
              disabled={disabled}
              options={NETWORKS}
              onChange={(network) => set({ network })}
            />
          </label>
          {(value.network === "ws" ||
            value.network === "http" ||
            value.network === "httpupgrade" ||
            value.network === "xhttp") && (
            <div className="field-grid">
              <label className="field">
                <span>路径</span>
                <input
                  value={value.path ?? ""}
                  onChange={(e) => set({ path: e.target.value })}
                  placeholder="/path"
                  disabled={disabled}
                />
              </label>
              <label className="field">
                <span>Host</span>
                <input
                  value={value.host ?? ""}
                  onChange={(e) => set({ host: e.target.value })}
                  disabled={disabled}
                />
              </label>
            </div>
          )}
          {value.network === "grpc" && (
            <label className="field">
              <span>Service Name</span>
              <input
                value={value.serviceName ?? ""}
                onChange={(e) => set({ serviceName: e.target.value })}
                disabled={disabled}
              />
            </label>
          )}
        </>
      )}
    </div>
  );
}

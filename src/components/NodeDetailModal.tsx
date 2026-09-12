import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { ProtocolConfig, ProxyNode, TransportDetail } from "../types";

function Row({ label, value }: { label: string; value: ReactNode }) {
  // Falsy values (empty optional fields) collapse so each protocol only
  // shows the rows it actually carries.
  if (value === undefined || value === null || value === "") return null;
  return (
    <div className="node-detail-row">
      <span className="node-detail-label">{label}</span>
      <span className="node-detail-value">{value}</span>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="node-detail-section">
      <h3>{title}</h3>
      <div className="node-detail-rows">{children}</div>
    </section>
  );
}

/** Protocol parameter rows — curated per protocol (labels that read as
 * protocol terms stay literal: UUID / SNI / ALPN / REALITY / PSK / MTU). */
function ProtoRows({ config }: { config: ProtocolConfig | undefined }) {
  const { t } = useI18n();
  if (!config) return null;
  switch (config.protocol) {
    case "shadowsocks":
      return (
        <>
          <Row label={t("nodes.fMethod")} value={config.method} />
          <Row label={t("nodes.fPassword")} value={config.password} />
          <Row label={t("nodes.fPlugin")} value={config.plugin} />
          <Row label={t("nodes.fPluginOpts")} value={config.plugin_opts} />
          {config.shadow_tls && (
            <>
              <Row label="shadow-tls" value={`v${config.shadow_tls.version}`} />
              <Row label="shadow-tls Host" value={config.shadow_tls.host} />
              <Row
                label={t("nodes.fPassword")}
                value={config.shadow_tls.password}
              />
            </>
          )}
        </>
      );
    case "vmess":
      return (
        <>
          <Row label="UUID" value={config.uuid} />
          <Row label="alterId" value={config.alter_id} />
          <Row label={t("nodes.fMethod")} value={config.security} />
        </>
      );
    case "vless":
      return (
        <>
          <Row label="UUID" value={config.uuid} />
          <Row label="Flow" value={config.flow} />
          <Row label="Packet Encoding" value={config.packet_encoding} />
        </>
      );
    case "trojan":
      return <Row label={t("nodes.fPassword")} value={config.password} />;
    case "hysteria2":
      return (
        <>
          <Row label={t("nodes.fPassword")} value={config.password} />
          <Row label={t("nodes.fObfs")} value={config.obfs} />
          <Row label={t("nodes.fObfsPass")} value={config.obfs_password} />
          <Row label={t("nodes.fUp")} value={config.up_mbps} />
          <Row label={t("nodes.fDown")} value={config.down_mbps} />
        </>
      );
    case "tuic":
      return (
        <>
          <Row label="UUID" value={config.uuid} />
          <Row label={t("nodes.fPassword")} value={config.password} />
          <Row label="Congestion Control" value={config.congestion_control} />
          <Row label="UDP Relay Mode" value={config.udp_relay_mode} />
          <Row label="Zero-RTT" value={t(config.zero_rtt_handshake ? "common.on" : "common.off")} />
        </>
      );
    case "socks5":
    case "http":
      return (
        <>
          <Row label={t("nodes.fUser")} value={config.username} />
          <Row label={t("nodes.fPassword")} value={config.password} />
          {config.protocol === "http" && (
            <Row label="Path" value={config.path} />
          )}
        </>
      );
    case "hysteria":
      return (
        <>
          <Row
            label={t("nodes.fAuth")}
            value={
              config.auth_base64 ? `${config.auth}（Base64）` : config.auth
            }
          />
          <Row label={t("nodes.fObfs")} value={config.obfs} />
          <Row label={t("nodes.fUp")} value={config.up_mbps} />
          <Row label={t("nodes.fDown")} value={config.down_mbps} />
        </>
      );
    case "shadowtls":
      return (
        <>
          <Row label="Version" value={`v${config.version}`} />
          <Row label={t("nodes.fPassword")} value={config.password} />
        </>
      );
    case "ssh":
      return (
        <>
          <Row label={t("nodes.fUser")} value={config.user} />
          <Row label={t("nodes.fPassword")} value={config.password} />
          <Row label="Private Key" value={config.private_key} />
          <Row
            label="Host Keys"
            value={config.host_key.join("\n")}
          />
        </>
      );
    case "naive":
      return (
        <>
          <Row label={t("nodes.fUser")} value={config.username} />
          <Row label={t("nodes.fPassword")} value={config.password} />
          <Row label="QUIC" value={t(config.quic ? "common.on" : "common.off")} />
        </>
      );
    case "tor":
      return (
        <>
          <Row label="Executable" value={config.executable_path} />
          <Row label="Extra Args" value={config.extra_args.join(" ")} />
          <Row label="Data Directory" value={config.data_directory} />
        </>
      );
    case "wireguard":
      return (
        <>
          <Row
            label="Local Address"
            value={config.local_address.join(", ")}
          />
          <Row label="Private Key" value={config.private_key} />
          <Row label="Peer Public Key" value={config.peer_public_key} />
          <Row label="Pre-shared Key" value={config.pre_shared_key} />
          <Row
            label="Reserved"
            value={config.reserved.length ? config.reserved.join(", ") : undefined}
          />
          <Row label="MTU" value={config.mtu} />
        </>
      );
    case "anytls":
      return <Row label={t("nodes.fPassword")} value={config.password} />;
    case "snell":
      return (
        <>
          <Row label="PSK" value={config.psk} />
          <Row label="Version" value={`v${config.version}`} />
          <Row label="User Key" value={config.userkey} />
          <Row label="Reuse" value={config.reuse === undefined ? undefined : t(config.reuse ? "common.on" : "common.off")} />
          <Row label={t("nodes.fObfs")} value={config.obfs_mode} />
          <Row label="Obfs Host" value={config.obfs_host} />
          <Row label="Mode" value={config.mode} />
        </>
      );
    case "masque":
      return (
        <>
          <Row label="Private Key" value={config.private_key} />
          <Row label="Public Key" value={config.public_key} />
          <Row label="IP" value={config.ip} />
          <Row label="IPv6" value={config.ipv6} />
          <Row label="MTU" value={config.mtu} />
          <Row label="Network" value={config.network} />
          <Row label="Congestion" value={config.congestion_controller} />
        </>
      );
    default:
      return null;
  }
}

function TransportRows({ transport }: { transport: TransportDetail | undefined }) {
  const { t } = useI18n();
  // Absent transport means plain TCP (the Rust default) — still shown so the
  // section is a complete picture of the wire shape.
  const effective: TransportDetail = transport ?? { type: "tcp" };
  return (
    <>
      <Row label={t("nodes.fTransportType")} value={effective.type.toUpperCase()} />
      {effective.type === "ws" && (
        <>
          <Row label="Path" value={effective.path} />
          <Row
            label="Headers"
            value={
              effective.headers
                ? Object.entries(effective.headers)
                    .map(([k, v]) => `${k}: ${v}`)
                    .join("\n")
                : undefined
            }
          />
          <Row label="Max Early Data" value={effective.max_early_data} />
        </>
      )}
      {effective.type === "grpc" && (
        <Row label="Service Name" value={effective.service_name} />
      )}
      {(effective.type === "http" ||
        effective.type === "httpupgrade" ||
        effective.type === "xhttp") && (
        <>
          <Row label="Path" value={effective.path} />
          <Row
            label="Host"
            value={Array.isArray(effective.host)
              ? effective.host.join(", ")
              : effective.host}
          />
        </>
      )}
      {effective.type === "xhttp" && <Row label="Mode" value={effective.mode} />}
    </>
  );
}

/**
 * Read-only node detail modal — the ⋮ menu "Details" action on node cards.
 * Renders per-protocol parameters (credentials, ciphers, obfs), the TLS
 * layer (SNI/ALPN/uTLS/REALITY) and the transport layer, from the full
 * ProxyNode payload that list_all_nodes already carries (serde-flattened).
 */
export function NodeDetailModal({
  node,
  onClose,
}: {
  node: ProxyNode;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const tls = node.tls;
  const hasTls = !!tls && (tls.enabled || !!tls.reality_public_key);
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal node-detail-modal" onClick={(e) => e.stopPropagation()}>
        <header className="modal-header">
          <h2>{t("nodes.detailTitle")}</h2>
          <button type="button" className="icon-btn" onClick={onClose}>
            ×
          </button>
        </header>
        <div className="modal-body">
          <div className="node-detail-name" title={node.name}>
            {node.name}
          </div>
          <Section title={t("nodes.detailBasic")}>
            <Row label={t("nodes.fProtocol")} value={node.protocol} />
            <Row label={t("nodes.fServer")} value={node.server} />
            <Row label={t("nodes.fPort")} value={node.port} />
            <Row
              label={t("nodes.fUdp")}
              value={node.udp === undefined ? undefined : t(node.udp ? "common.on" : "common.off")}
            />
            <Row label={t("nodes.fSub")} value={node.subscription_name} />
            <Row label={t("nodes.fSource")} value={node.source} />
          </Section>
          <Section title={t("nodes.detailProto")}>
            <ProtoRows config={node.config} />
          </Section>
          {hasTls && (
            <Section title={t("nodes.detailTls")}>
              <Row label="SNI" value={tls?.server_name} />
              <Row label="ALPN" value={tls?.alpn?.join(", ")} />
              <Row label="uTLS" value={tls?.utls_fingerprint} />
              <Row
                label={t("nodes.fSkipCert")}
                value={
                  tls?.insecure === undefined
                    ? undefined
                    : t(tls.insecure ? "common.on" : "common.off")
                }
              />
              <Row label="REALITY Public Key" value={tls?.reality_public_key} />
              <Row label="REALITY Short ID" value={tls?.reality_short_id} />
            </Section>
          )}
          <Section title={t("nodes.detailTransport")}>
            <TransportRows transport={node.transport} />
          </Section>
        </div>
      </div>
    </div>
  );
}

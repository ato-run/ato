# MagNet Discovery (Draft)

**Status:** v0.1  
**最終更新:** 2026-01-23

## 1. 目的
- LAN/Global の二層Discovery
 - P2P での到達性と可用性を確保

## 2. Local Discovery
### 2.1 mDNS
- サービス名: `_p2p._udp.local`
- TXT レコードに `dnsaddr=/.../p2p/<peer_id>` を格納

### 2.2 Bluetooth（将来）
- 近接検出の補助チャネル

## 3. Global Discovery
### 3.1 Kademlia DHT
- 既定パラメータ: `k=20`, `α=10`
- Client/Server モードを区別
	- 公開ノード: server
	- NAT/モバイル: client

### 3.2 Provider Record
- `ADD_PROVIDER` / `GET_PROVIDERS` を使用
- Provider Record は再公開（Republish）と期限（Expiration）を設定

## 4. Sync
### 4.1 GossipSub
- CRDT 更新の配布
- Message signing ポリシーは `StrictSign` を推奨

## 5. Relay
### 5.1 Circuit Relay / DCUtR
- NAT越え失敗時のフォールバック
- DCUtR で direct 接続への昇格を試行

## 6. 未決事項
### 6.1 インセンティブ
- ボランティア運用かスコアリングか

### 6.2 モバイル最適化
- バッテリー消費と接続戦略

### 6.3 Provider Record TTL
- Republish/Expiration の初期値

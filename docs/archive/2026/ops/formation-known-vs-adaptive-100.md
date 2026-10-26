# Formation known-D vs adaptive — paired100

**baseline7 → adaptive7、追加PASS0。** source100件は同じ。既存K19件同一、81件は承認済みの静的K明示追加で、model-driven K gainは0。known-D再測定は100/100 terminal／D ref一致。generated D10（baseline Dなしから9）、新規execution2、generated-D PASS0、regression0。

表のgenerated DはValidator登録済み、executionはactual Runtime attestationを要する。catalog gapはroot-only authorizationによる公開catalogの空集合という範囲つき分類。known phase→zero-known-D Requesterの2段で、前段refusalは別rawにあり、第二model contextへは入っていない。API searchは12件のみ。

| # | app | baseline terminal | adaptive terminal | explicit K追加 | generated D | 新規execution | calls CP/DP |
|---|---|---|---|---|---:|---|---|
| 1 | Excalidraw | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 2 | WBO | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 3 | Uptime Kuma | fully_satisfied | known_d_pass | no | 0 | no | 0/0 |
| 4 | FreshRSS | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 5 | linkding | network_denied | network_denied | no | 1 | no | 1/1 |
| 6 | SearXNG | preset_node_static_needs_lockfile | network_denied | yes | 1 | no | 1/1 |
| 7 | changedetection.io | preset_no_match | network_denied | yes | 1 | no | 1/1 |
| 8 | Etherpad | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 9 | Miniflux | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 10 | Gitea | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 11 | Vikunja | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 12 | Mealie | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 13 | HedgeDoc | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 14 | Actual Budget | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 15 | JupyterLab | preset_node_static_v2_workspace | proposal_declined | yes | 0 | no | 1/1 |
| 16 | LibreChat | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 17 | ComfyUI | preset_no_match | network_denied | yes | 1 | no | 1/1 |
| 18 | PdfDing | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 19 | Grist core | preset_node_static_v2_manager_config | operation_catalog_gap | yes | 0 | no | 0/0 |
| 20 | File Browser | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 21 | 2048 | fully_satisfied | known_d_pass | no | 0 | no | 0/0 |
| 22 | reveal.js | fully_satisfied | known_d_pass | no | 0 | no | 0/0 |
| 23 | CyberChef | intent_malformed | operation_catalog_gap | yes | 0 | no | 0/0 |
| 24 | IT-Tools | preset_node_static_v2_static_unproven | operation_catalog_gap | yes | 0 | no | 0/0 |
| 25 | Squoosh | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 26 | Hoppscotch | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 27 | Wiki.js | preset_node_static_v2_manager_config | operation_catalog_gap | yes | 0 | no | 0/0 |
| 28 | Kutt | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 29 | Umami | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 30 | Ghost | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 31 | Radicale | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 32 | Calibre-Web | preset_no_match | network_denied | yes | 1 | no | 1/1 |
| 33 | Shynet | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 34 | text-generation-webui | preset_no_match | attempt_failed | yes | 1 | yes | 1/1 |
| 35 | PrivateGPT | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 36 | Gotify | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 37 | Navidrome | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 38 | PocketBase | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 39 | Glance | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 40 | Shiori | preset_node_static_v2_bun_deferred | operation_catalog_gap | yes | 0 | no | 0/0 |
| 41 | Vaultwarden | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 42 | Lemmy | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 43 | Paperless-ngx | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 44 | Mastodon | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 45 | Linkwarden | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 46 | NetBox | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 47 | BookStack | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 48 | Firefly III | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 49 | OpenRefine | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 50 | Jellyfin | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 51 | A Dark Room | preset_node_static_v2_static_unproven | operation_catalog_gap | yes | 0 | no | 0/0 |
| 52 | Hextris | fully_satisfied | known_d_pass | no | 0 | no | 0/0 |
| 53 | 0h h1 | fully_satisfied | known_d_pass | no | 0 | no | 0/0 |
| 54 | 0h n0 | fully_satisfied | known_d_pass | no | 0 | no | 0/0 |
| 55 | Emoji search | fully_satisfied | known_d_pass | no | 0 | no | 0/0 |
| 56 | AudioMass | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 57 | SVGOMG | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 58 | quiver | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 59 | hello wordl | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 60 | diagrams.net | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 61 | Node-RED | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 62 | SillyTavern | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 63 | Dockge | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 64 | Wekan | preset_node_static_needs_build_script | attempt_failed | yes | 1 | yes | 1/1 |
| 65 | Homepage | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 66 | Audiobookshelf | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 67 | Misskey | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 68 | SilverBullet | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 69 | copyparty | preset_no_match | attempt_failed | yes | 1 | no | 1/1 |
| 70 | Healthchecks | preset_no_match | network_denied | yes | 1 | no | 1/1 |
| 71 | ArchiveBox | preset_no_match | network_denied | yes | 1 | no | 1/1 |
| 72 | Whoogle | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 73 | Superset | preset_no_match | source_transport_limit | yes | 0 | no | 0/0 |
| 74 | Datasette | preset_node_static_needs_build_script | operation_catalog_gap | yes | 0 | no | 0/0 |
| 75 | Home Assistant | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 76 | mitmproxy | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 77 | Memos | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 78 | Gatus | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 79 | MinIO | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 80 | AdGuard Home | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 81 | Beszel | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 82 | Sonic | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 83 | RustDesk Server | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 84 | Atuin | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 85 | Redlib | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 86 | wallabag | preset_node_static_v2_static_unproven | operation_catalog_gap | yes | 0 | no | 0/0 |
| 87 | Lychee | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 88 | grocy | preset_node_static_v2_manager_config | operation_catalog_gap | yes | 0 | no | 0/0 |
| 89 | FOSSBilling | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 90 | Discourse | preset_node_static_v2_workspace | operation_catalog_gap | yes | 0 | no | 0/0 |
| 91 | Huginn | network_denied | operation_catalog_gap | no | 0 | no | 0/0 |
| 92 | Redmine | preset_node_static_v2_static_unproven | operation_catalog_gap | yes | 0 | no | 0/0 |
| 93 | Jenkins | preset_node_static_v2_manager_config | proposal_declined | yes | 0 | no | 1/1 |
| 94 | Zeppelin | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 95 | NiFi | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 96 | Guacamole | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 97 | Kavita | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 98 | Jackett | preset_no_match | operation_catalog_gap | yes | 0 | no | 0/0 |
| 99 | Radarr | preset_node_static_v2_manager_config | operation_catalog_gap | yes | 0 | no | 0/0 |
| 100 | Sonarr | preset_node_static_v2_manager_config | operation_catalog_gap | yes | 0 | no | 0/0 |

[完全比較JSON](formation-known-vs-adaptive-100.json)は全source pin、before/after K/D refs、funnel、reach booleansを保持する。[結果・cost・証拠](formation-adaptive-100.md)。historical baseline／functional appendを書き換えていない。

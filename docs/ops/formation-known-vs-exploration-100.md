# Known-D baseline versus bounded exploration

Frozen sources unchanged: baseline 7/100 → exploration 7/100 (+0). Regressed typed-K apps: []. New typed-K apps: [].

K/template entry differences, new adapter code and exploration ceiling are documented in [measurement](formation-exploration-100.md). Outcome equality does not mean diagnostic or execution reach equality. The JSON records original/current K/D refs and reach; historical baseline is not overwritten.

Reach improved on 81 apps and reduced on 12 apps. Existing verified PASS apps are preserved; losslessly unrepresentable legacy authoring D is skipped in this exploration entry, so D availability is not claimed regression-free. Superset's source preparation boundary is specific to this selected directory materialization.

| Index | App | Baseline terminal | Exploration terminal | Typed-K | CP/DP calls | Improved / reduced layers |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Excalidraw | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 2 | WBO | preset_node_static_needs_build_script | no_progress | False → False | 2/0 | B,C,D,E /  |
| 3 | Uptime Kuma | fully_satisfied | submitted | True → True | 0/0 |  /  |
| 4 | FreshRSS | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 5 | linkding | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 6 | SearXNG | preset_node_static_needs_lockfile | rounds_exhausted | False → False | 3/0 | B,C /  |
| 7 | changedetection.io | preset_no_match | rounds_exhausted | False → False | 3/0 | B,C,D,E /  |
| 8 | Etherpad | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 9 | Miniflux | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 10 | Gitea | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 11 | Vikunja | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 12 | Mealie | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 13 | HedgeDoc | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 14 | Actual Budget | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 15 | JupyterLab | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 16 | LibreChat | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 17 | ComfyUI | preset_no_match | rounds_exhausted | False → False | 2/0 | B /  |
| 18 | PdfDing | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 19 | Grist core | preset_node_static_v2_manager_config | rounds_exhausted | False → False | 3/0 | B /  |
| 20 | File Browser | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 21 | 2048 | fully_satisfied | submitted | True → True | 0/0 |  /  |
| 22 | reveal.js | fully_satisfied | submitted | True → True | 0/0 |  /  |
| 23 | CyberChef | intent_malformed | rounds_exhausted | False → False | 3/0 | B /  |
| 24 | IT-Tools | preset_node_static_v2_static_unproven | rounds_exhausted | False → False | 3/0 | B /  |
| 25 | Squoosh | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 26 | Hoppscotch | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 27 | Wiki.js | preset_node_static_v2_manager_config | rounds_exhausted | False → False | 3/0 | B,C /  |
| 28 | Kutt | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B,C,D,E /  |
| 29 | Umami | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 30 | Ghost | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 31 | Radicale | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 32 | Calibre-Web | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 33 | Shynet | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 34 | text-generation-webui | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 35 | PrivateGPT | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 36 | Gotify | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 37 | Navidrome | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 38 | PocketBase | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 39 | Glance | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 40 | Shiori | preset_node_static_v2_bun_deferred | rounds_exhausted | False → False | 3/0 | B /  |
| 41 | Vaultwarden | preset_no_match | exploration_authority_exceeded | False → False | 2/0 | B /  |
| 42 | Lemmy | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 43 | Paperless-ngx | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 44 | Mastodon | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 45 | Linkwarden | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 46 | NetBox | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 47 | BookStack | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 48 | Firefly III | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 49 | OpenRefine | preset_no_match | rounds_exhausted | False → False | 3/0 | B,C /  |
| 50 | Jellyfin | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 51 | A Dark Room | preset_node_static_v2_static_unproven | rounds_exhausted | False → False | 3/0 | B /  |
| 52 | Hextris | fully_satisfied | submitted | True → True | 0/0 |  /  |
| 53 | 0h h1 | fully_satisfied | submitted | True → True | 0/0 |  /  |
| 54 | 0h n0 | fully_satisfied | submitted | True → True | 0/0 |  /  |
| 55 | Emoji search | fully_satisfied | submitted | True → True | 0/0 |  /  |
| 56 | AudioMass | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 57 | SVGOMG | network_denied | rounds_exhausted | False → False | 3/1 | D,E /  |
| 58 | quiver | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 59 | hello wordl | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 60 | diagrams.net | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 61 | Node-RED | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 62 | SillyTavern | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 63 | Dockge | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 64 | Wekan | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 65 | Homepage | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 66 | Audiobookshelf | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 67 | Misskey | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 68 | SilverBullet | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 69 | copyparty | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 70 | Healthchecks | preset_no_match | no_progress | False → False | 3/0 | B,C,D,E /  |
| 71 | ArchiveBox | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 72 | Whoogle | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 73 | Superset | preset_no_match | source_archive_cap_exceeded | False → False | 0/0 |  / A |
| 74 | Datasette | preset_node_static_needs_build_script | rounds_exhausted | False → False | 3/0 | B /  |
| 75 | Home Assistant | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 76 | mitmproxy | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 77 | Memos | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 78 | Gatus | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 79 | MinIO | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 80 | AdGuard Home | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 81 | Beszel | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 82 | Sonic | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 83 | RustDesk Server | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 84 | Atuin | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 85 | Redlib | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 86 | wallabag | preset_node_static_v2_static_unproven | rounds_exhausted | False → False | 3/0 | B /  |
| 87 | Lychee | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 88 | grocy | preset_node_static_v2_manager_config | rounds_exhausted | False → False | 3/0 | B /  |
| 89 | FOSSBilling | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 90 | Discourse | preset_node_static_v2_workspace | rounds_exhausted | False → False | 3/0 | B /  |
| 91 | Huginn | network_denied | rounds_exhausted | False → False | 3/0 |  / C |
| 92 | Redmine | preset_node_static_v2_static_unproven | rounds_exhausted | False → False | 3/0 | B /  |
| 93 | Jenkins | preset_node_static_v2_manager_config | rounds_exhausted | False → False | 3/0 | B /  |
| 94 | Zeppelin | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 95 | NiFi | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 96 | Guacamole | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 97 | Kavita | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 98 | Jackett | preset_no_match | rounds_exhausted | False → False | 3/0 | B /  |
| 99 | Radarr | preset_node_static_v2_manager_config | rounds_exhausted | False → False | 3/0 | B /  |
| 100 | Sonarr | preset_node_static_v2_manager_config | rounds_exhausted | False → False | 3/0 | B /  |


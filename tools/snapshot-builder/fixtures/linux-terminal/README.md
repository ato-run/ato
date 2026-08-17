# Linux terminal fixture

Terminal Surface v1 の決定論的な single-service fixture。stdlib Python の一つの
workload が `/health` を提供しながら、制御 PTY 上で次を確認できる。

- ANSI color、cursor positioning、alternate screen
- keyboard echo
- `SIGWINCH` 後の `resize:<cols>x<rows>`
- Ctrl+C の clean exit
- `exit` による code 0 終了

外部 shell は起動せず、Capsule が宣言した `terminal_fixture.py` だけを実行する。
Terminal transcript は保存しない。

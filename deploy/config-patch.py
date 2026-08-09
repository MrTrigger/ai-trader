import re, pathlib
p = pathlib.Path("/app/config.toml"); s = p.read_text()
s = re.sub(r'^model_path = .*$', 'model_path = "/app/ranker-rank-1.json"', s, flags=re.M)
p.write_text(s)

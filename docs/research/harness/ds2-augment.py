R=/home/magnus/dev/magnus/ai-trader/var/research/recovered
# extract the merge script body
python3 - <<'PY'
s=open("/home/magnus/dev/magnus/ai-trader/var/research/recovered/ds4-writer-1.txt").read()
body=s.split("<<'PYEOF'\n",1)[1].rsplit("\nPYEOF",1)[0]
open("/home/magnus/dev/magnus/ai-trader/var/research/recovered/merge_ds4.py","w").write(body)
print(len(body),"bytes")
PY
grep -n "sys.argv\|out =\|ds2\|write_parquet" $R/dataset.py | head -6
grep -n "ds2\|ds3\|write_parquet" $R/merge_ds4.py | head -8
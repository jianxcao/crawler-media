#!/bin/zsh
# 启动隔离压测实例：zsh stress-up.sh [root=/tmp/e2e-stress]；token 来自 $CRAWLER_MEDIA_TOKEN（默认 changeme，只在本机）
set -e; R=${1:-/tmp/e2e-stress}; P=${0:A:h}/../..; mkdir -p $R/data
[ -d $R/media ] || python3 ${0:A:h}/gen-stress.py $R/media
CRAWLER_MEDIA_DATA=$R/data CRAWLER_MEDIA_ALLOW_MULTI_INSTANCE=1 CRAWLER_MEDIA_LISTEN=127.0.0.1:18766 \
CRAWLER_MEDIA_TOKEN=${CRAWLER_MEDIA_TOKEN:-changeme} CRAWLER_MEDIA_ADMIN_PASSWORD=test-admin-secret-password \
nohup $P/target/debug/crawler-media serve > $R/backend.log 2>&1 & echo $! > $R/pid; echo "pid $(cat $R/pid)"
# 建库：POST /api/v1/libraries {name:stress-movie, kind:movie, paths:[$R/media/movie]}、stress-tv 同理（字段以 routes / web 表单为准），然后 python3 stressbench.py 40 idle
# 停止：kill $(cat $R/pid); rm -rf $R

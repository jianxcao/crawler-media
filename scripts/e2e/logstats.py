import re,statistics
L=open('/Users/jianxcao/work/fun/crawler-media/.test-logs/backend.log',errors='ignore').read().splitlines()
def st(name,vals):
    v=sorted(vals)
    if not v: print(name,'n=0');return
    print('%-28s n=%4d p50=%8.0f p95=%8.0f max=%8.0f'%(name,len(v),statistics.median(v),v[min(len(v)-1,int(len(v)*.95))],v[-1]))
def grab(filt,key,scale=1):
    out=[]
    for l in L:
        if filt in l:
            m=re.search(r'\b'+key+r'=(?:Some\()?([0-9.]+)',l)
            if m: out.append(float(m.group(1))*scale)
    return out
T='阶段耗时汇总'
st('media_info_elapsed_ms',[x for x in grab(T,'media_info_elapsed_ms') if x>0])
st('intro_elapsed_ms',[x for x in grab(T,'intro_elapsed_ms') if x>0])
st('outro_elapsed_ms',[x for x in grab(T,'outro_elapsed_ms') if x>0])
st('comparison_elapsed_ms',[x for x in grab(T,'comparison_elapsed_ms') if x>0])
st('queue_wait_ms',grab(T,'queue_wait_ms'))
C='Chromaprint 阶段耗时拆分'
st('time_to_first_pcm_ms',grab(C,'time_to_first_pcm_ms'))
st('pcm_read_wait_ms(network)',grab(C,'pcm_read_wait_us',0.001))
st('chromaprint_consume_ms(cpu)',grab(C,'chromaprint_consume_us',0.001))
st('pcm_stream_elapsed_ms',grab(C,'pcm_stream_elapsed_ms'))
st('remote_read_waited_ms',grab('Remote media read admitted','waited_ms'))
print('redirect resolved',sum('STRM redirect resolved' in l for l in L),'remote reads',sum('Remote media read admitted' in l for l in L))
lat=[(l[:19],re.search(r'route=(\S+)',l).group(1),int(re.search(r'headers_latency_ms=(\d+)',l).group(1))) for l in L if 'HTTP 请求缓慢' in l]
import collections;c=collections.Counter(r for _,r,_ in lat);print('slow http',len(lat),c.most_common(8))
print('slow detail',[x for x in lat if x[1].endswith('{item_id}') ][:10])
print('5xx',collections.Counter(re.search(r'route=(\S+)',l).group(1) for l in L if '5xx' in l))

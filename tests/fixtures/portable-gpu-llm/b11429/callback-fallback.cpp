#include <cstdio>
#include <cstdlib>
enum ggml_log_level { GGML_LOG_LEVEL_DEBUG=1, GGML_LOG_LEVEL_INFO=2 };
struct common_log;
void common_log_set_verbosity_thold(int);
void common_log_set_jsonl(bool);
void common_log_default_callback(ggml_log_level, const char *, void *);
common_log * common_log_main();
void common_log_flush(common_log *);
int main(int argc, char ** argv) {
 if (argc!=4) return 2;
 common_log_set_verbosity_thold(std::atoi(argv[1]));
 common_log_set_jsonl(std::atoi(argv[2])!=0);
 int gpu=std::atoi(argv[3]); char b[512];
 snprintf(b,sizeof b,"%s: n_layer_all           = %u\n","print_info",28u);
 common_log_default_callback(GGML_LOG_LEVEL_INFO,b,nullptr);
 for (int i=0;i<29;i++) {
  snprintf(b,sizeof b,"load_tensors: layer %3d assigned to device %s, is_swa = %d\n",i,i<gpu?"CUDA0":"CPU",0);
  common_log_default_callback(GGML_LOG_LEVEL_DEBUG,b,nullptr);
 }
 snprintf(b,sizeof b,"%s: tensor '%s' (%s) (and %zu others) cannot be used with preferred buffer type %s, using %s instead\n","done_getting_tensors","blk.0.attn_q.weight","q4_K",0ul,"CUDA0","CPU");
 common_log_default_callback(GGML_LOG_LEVEL_DEBUG,b,nullptr);
 snprintf(b,sizeof b,"%s: offloaded %d/%d layers to GPU\n","load_tensors",gpu,29);
 common_log_default_callback(GGML_LOG_LEVEL_INFO,b,nullptr);
 common_log_flush(common_log_main());
 return 0;
}

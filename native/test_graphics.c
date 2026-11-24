/* In-process test of the real FreeRDP EGFX/GDI callbacks. Loopback handshake for real GDI/cache setup; no screen capture. */
#include "rdp_bridge.c"
#include <assert.h>
#include <unistd.h>
typedef struct { unsigned count; BYTE expected; int busy; char clipboard[128]; unsigned formats; int file_mode; } Probe;
static int capture(void* opaque,const uint8_t* data,uint32_t w,uint32_t h,uint32_t stride){
    Probe* p=opaque;
    if(p->busy)return 2;
    assert(w==640&&h==480);
    for(UINT32 y=0;y<h;y++)for(UINT32 x=0;x<w;x++)assert(data[(size_t)y*stride+x*4]==p->expected);
    p->count++;return 1;
}
static int cert(void* a,int b,const char* c,uint16_t d,const char* e,const char* f,const char* g,const char* h,const char* i){
    (void)a;(void)b;(void)c;(void)d;(void)e;(void)f;(void)g;(void)h;(void)i;return 1;
}
static void clipboard(void* opaque,const char* text){Probe* p=opaque;snprintf(p->clipboard,sizeof(p->clipboard),"%s",text);}
static UINT clip_caps(CliprdrClientContext* ctx,const CLIPRDR_CAPABILITIES* caps){(void)ctx;assert(caps->cCapabilitiesSets==1);return CHANNEL_RC_OK;}
static UINT clip_list(CliprdrClientContext* ctx,const CLIPRDR_FORMAT_LIST* list){Probe* p=((RdpBridge*)ctx->custom)->opaque;p->formats=list->numFormats;return CHANNEL_RC_OK;}
static UINT clip_list_reply(CliprdrClientContext* ctx,const CLIPRDR_FORMAT_LIST_RESPONSE* response){(void)ctx;assert(response->common.msgFlags==CB_RESPONSE_OK);return CHANNEL_RC_OK;}
static UINT clip_data_request(CliprdrClientContext* ctx,const CLIPRDR_FORMAT_DATA_REQUEST* request){(void)ctx;assert(request->requestedFormatId==CF_UNICODETEXT);return CHANNEL_RC_OK;}
static UINT clip_data_reply(CliprdrClientContext* ctx,const CLIPRDR_FORMAT_DATA_RESPONSE* response){Probe* p=((RdpBridge*)ctx->custom)->opaque;assert(response->common.msgFlags==CB_RESPONSE_OK);if(p->file_mode){FILEDESCRIPTORW* files=NULL;UINT32 count=0;assert(cliprdr_parse_file_list(response->requestedFormatData,response->common.dataLen,&files,&count)==0);assert(count==2);assert(files[0].dwFileAttributes&FILE_ATTRIBUTE_DIRECTORY);assert(files[1].nFileSizeLow==3);assert(files[1].cFileName[0]=='f');free(files);}else{assert(response->common.dataLen==14);assert(((WCHAR*)response->requestedFormatData)[0]==0x41f);}return CHANNEL_RC_OK;}
static UINT clip_file_reply(CliprdrClientContext* ctx,const CLIPRDR_FILE_CONTENTS_RESPONSE* response){(void)ctx;assert(response->common.msgFlags==CB_RESPONSE_OK);assert(response->streamId==7);if(response->cbRequested==3)assert(memcmp(response->requestedData,"abc",3)==0);else{assert(response->cbRequested==8);assert(response->requestedData[0]==3);}return CHANNEL_RC_OK;}
static void fill(RdpgfxClientContext* gfx,UINT16 top,UINT16 bottom,BYTE value){
    RECTANGLE_16 rect={0,top,640,bottom};
    RDPGFX_SOLID_FILL_PDU cmd={.surfaceId=1,.fillRectCount=1,.fillRects=&rect};
    cmd.fillPixel.B=value;cmd.fillPixel.R=0x33;cmd.fillPixel.G=0x22;cmd.fillPixel.XA=255;
    assert(gfx->SolidFill(gfx,&cmd)==CHANNEL_RC_OK);
}
static UINT set_surface(RdpgfxClientContext* gfx,UINT16 id,void* data){assert(id==1);*(void**)gfx->handle=data;return 0;}
static void* get_surface(RdpgfxClientContext* gfx,UINT16 id){assert(id==1);return *(void**)gfx->handle;}
static UINT get_ids(RdpgfxClientContext* gfx,UINT16** ids,UINT16* count){
    *count=*(void**)gfx->handle?1:0;*ids=NULL;
    if(*count){*ids=malloc(sizeof(UINT16));assert(*ids);**ids=1;}return 0;
}
int main(int argc,char** argv){
    assert(argc==2);
    assert(rdp_manager_initialize());
    Probe probe={.expected=0x11};
    RdpBridge* bridge=rdp_manager_new("127.0.0.1",(uint16_t)atoi(argv[1]),"fixture","fixture-password","","",640,480,0x419,3,&probe,capture,cert,clipboard);
    assert(bridge&&rdp_manager_connect(bridge));
    RdpBridge* compatibility=rdp_manager_new("127.0.0.1",(uint16_t)atoi(argv[1]),"fixture","fixture-password","","",640,480,0x419,2,&probe,capture,cert,clipboard);
    assert(compatibility);
    assert(freerdp_settings_get_bool(compatibility->instance->context->settings,FreeRDP_SoftwareGdi));
    if(strstr(freerdp_get_build_config(),"WITH_GFX_H264=ON"))assert(freerdp_settings_get_bool(compatibility->instance->context->settings,FreeRDP_GfxH264));
    rdp_manager_free(compatibility);
    RdpBridge* avc444=rdp_manager_new("127.0.0.1",(uint16_t)atoi(argv[1]),"fixture","fixture-password","","",640,480,0x419,4,&probe,capture,cert,clipboard);
    assert(avc444);
    assert(freerdp_settings_get_uint32(avc444->instance->context->settings,FreeRDP_GfxCapsFilter)==(1u<<1));
    assert(freerdp_settings_get_bool(avc444->instance->context->settings,FreeRDP_SupportGraphicsPipeline));
    RdpBridge* shared=rdp_manager_new("127.0.0.1",(uint16_t)atoi(argv[1]),"fixture","fixture-password","","/tmp",640,480,0x419,4,&probe,capture,cert,clipboard);
    assert(shared);
    assert(freerdp_settings_get_bool(shared->instance->context->settings,FreeRDP_DeviceRedirection));
    assert(freerdp_device_collection_find_type(shared->instance->context->settings,RDPDR_DTYP_FILESYSTEM));
    rdp_manager_free(shared);
    CliprdrClientContext clip={.custom=avc444,.ClientCapabilities=clip_caps,.ClientFormatList=clip_list,
        .ClientFormatListResponse=clip_list_reply,.ClientFormatDataRequest=clip_data_request,.ClientFormatDataResponse=clip_data_reply,.ClientFileContentsResponse=clip_file_reply};
    avc444->clipboard=&clip;
    CLIPRDR_MONITOR_READY monitor={0};
    assert(clipboard_ready(&clip,&monitor)==CHANNEL_RC_OK&&probe.formats==0);
    assert(rdp_manager_clipboard_set_text(avc444,"Привет")&&probe.formats==1);
    CLIPRDR_FORMAT_DATA_REQUEST request={.requestedFormatId=CF_UNICODETEXT};
    assert(clipboard_server_request(&clip,&request)==CHANNEL_RC_OK);
    CLIPRDR_FORMAT format={.formatId=CF_UNICODETEXT};
    CLIPRDR_FORMAT_LIST list={.numFormats=1,.formats=&format};
    assert(clipboard_server_list(&clip,&list)==CHANNEL_RC_OK);
    WCHAR remote[]={0x41f,0x440,0x438,0x432,0x435,0x442,0};
    CLIPRDR_FORMAT_DATA_RESPONSE reply={.common={.msgFlags=CB_RESPONSE_OK,.dataLen=sizeof(remote)},.requestedFormatData=(BYTE*)remote};
    assert(clipboard_server_data(&clip,&reply)==CHANNEL_RC_OK&&strcmp(probe.clipboard,"Привет")==0);
    char name[]="/tmp/rdp-clip-test-XXXXXX";
    int fd=mkstemp(name);assert(fd>=0);assert(write(fd,"abc",3)==3);close(fd);
    RdpClipboardFile files[]={ {"/tmp","folder",0,1},{name,"folder\\file.txt",3,0} };
    probe.file_mode=1;
    assert(rdp_manager_clipboard_set_files(avc444,files,2)&&probe.formats==1);
    CLIPRDR_FORMAT_DATA_REQUEST file_list={.requestedFormatId=0xC001};
    assert(clipboard_server_request(&clip,&file_list)==CHANNEL_RC_OK);
    CLIPRDR_FILE_CONTENTS_REQUEST file_size={.streamId=7,.listIndex=1,.dwFlags=FILECONTENTS_SIZE};
    assert(clipboard_file_request(&clip,&file_size)==CHANNEL_RC_OK);
    CLIPRDR_FILE_CONTENTS_REQUEST file_range={.streamId=7,.listIndex=1,.dwFlags=FILECONTENTS_RANGE,.cbRequested=3};
    assert(clipboard_file_request(&clip,&file_range)==CHANNEL_RC_OK);
    unlink(name);
    avc444->clipboard=NULL;
    rdp_manager_free(avc444);
    rdp_manager_prepare_input(bridge);
    assert(WaitForSingleObject(bridge->input_event,0)==WAIT_TIMEOUT);
    rdp_manager_wake(bridge);
    assert(WaitForSingleObject(bridge->input_event,0)==WAIT_OBJECT_0);
    rdp_manager_prepare_input(bridge);
    assert(WaitForSingleObject(bridge->input_event,0)==WAIT_TIMEOUT);
    rdpContext* ctx=bridge->instance->context;
    assert(!freerdp_settings_get_bool(ctx->settings,FreeRDP_GfxH264));
    assert(!freerdp_settings_get_bool(ctx->settings,FreeRDP_GfxAVC444));
    assert(!freerdp_settings_get_bool(ctx->settings,FreeRDP_SupportGraphicsPipeline));
    assert(!freerdp_settings_get_bool(ctx->settings,FreeRDP_RemoteFxCodec));
    assert(!freerdp_settings_get_bool(ctx->settings,FreeRDP_NSCodec));
    void* surface_data=NULL;
    RdpgfxClientContext context={.handle=&surface_data,.SetSurfaceData=set_surface,.GetSurfaceData=get_surface,.GetSurfaceIds=get_ids};
    RdpgfxClientContext* gfx=&context;
    ChannelConnectedEventArgs connected={0};connected.name=RDPGFX_DVC_CHANNEL_NAME;connected.pInterface=gfx;
    channel_connected(ctx,&connected);assert(bridge->gfx==gfx);
    RDPGFX_CREATE_SURFACE_PDU surface={.surfaceId=1,.width=640,.height=480,.pixelFormat=GFX_PIXEL_FORMAT_XRGB_8888};
    assert(gfx->CreateSurface(gfx,&surface)==CHANNEL_RC_OK);
    RDPGFX_MAP_SURFACE_TO_OUTPUT_PDU map={.surfaceId=1};
    assert(gfx->MapSurfaceToOutput(gfx,&map)==CHANNEL_RC_OK);
    RDPGFX_START_FRAME_PDU start={.frameId=1};RDPGFX_END_FRAME_PDU end={.frameId=1};
    assert(gfx->StartFrame(gfx,&start)==CHANNEL_RC_OK);
    fill(gfx,0,480,0x11);
    assert(publish_frame(bridge,FALSE)&&probe.count==0);
    assert(gfx->EndFrame(gfx,&end)==CHANNEL_RC_OK);
    assert(probe.count==1); /* Must publish before a following StartFrame in the same batch. */
    start.frameId=end.frameId=2;
    assert(gfx->StartFrame(gfx,&start)==CHANNEL_RC_OK);
    fill(gfx,0,240,0x44);
    assert(publish_frame(bridge,TRUE)&&probe.count==1);
    fill(gfx,240,480,0x44);probe.expected=0x44;probe.busy=1;
    assert(gfx->EndFrame(gfx,&end)==CHANNEL_RC_OK&&probe.count==1);
    assert(atomic_load(&bridge->frame_dirty));
    probe.busy=0;
    assert(publish_frame(bridge,TRUE)&&probe.count==2);
    assert(!atomic_load(&bridge->frame_dirty));
    RDPGFX_DELETE_SURFACE_PDU remove={.surfaceId=1};assert(gfx->DeleteSurface(gfx,&remove)==CHANNEL_RC_OK);
    ChannelDisconnectedEventArgs disconnected={0};disconnected.name=RDPGFX_DVC_CHANNEL_NAME;disconnected.pInterface=gfx;
    channel_disconnected(ctx,&disconnected);

    rdp_manager_free(bridge);
    puts("EGFX: full frames, immediate frame-boundary publication and final-frame retry OK");
    return 0;
}

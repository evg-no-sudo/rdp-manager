/* In-process test of the real FreeRDP EGFX/GDI callbacks. Loopback handshake for real GDI/cache setup; no screen capture. */
#include "rdp_bridge.c"
#include <assert.h>
typedef struct { unsigned count; BYTE expected; int busy; } Probe;
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
    RdpBridge* bridge=rdp_manager_new("127.0.0.1",(uint16_t)atoi(argv[1]),"fixture","fixture-password","",640,480,0x419,1,&probe,capture,cert);
    assert(bridge&&rdp_manager_connect(bridge));
    rdp_manager_prepare_input(bridge);
    assert(WaitForSingleObject(bridge->input_event,0)==WAIT_TIMEOUT);
    rdp_manager_wake(bridge);
    assert(WaitForSingleObject(bridge->input_event,0)==WAIT_OBJECT_0);
    rdp_manager_prepare_input(bridge);
    assert(WaitForSingleObject(bridge->input_event,0)==WAIT_TIMEOUT);
    rdpContext* ctx=bridge->instance->context;
    assert(!freerdp_settings_get_bool(ctx->settings,FreeRDP_GfxH264));
    assert(!freerdp_settings_get_bool(ctx->settings,FreeRDP_GfxAVC444));
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

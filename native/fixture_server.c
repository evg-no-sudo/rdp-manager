/* Synthetic loopback-only FreeRDP peer. No screen capture or OS login. */
#include <freerdp/freerdp.h>
#include <freerdp/peer.h>
#include <freerdp/settings.h>
#include <freerdp/input.h>
#include <freerdp/update.h>
#include <freerdp/channels/wtsvc.h>
#include <freerdp/channels/channels.h>
#include <freerdp/server/rdpgfx.h>
#include <freerdp/crypto/certificate.h>
#include <freerdp/crypto/privatekey.h>
#include <winpr/synch.h>
#include <winpr/sysinfo.h>
#include <winpr/ssl.h>
#include <winpr/ntlm.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
typedef struct {FILE* log;BOOL activated;BOOL painted;UINT64 end_at;BOOL gfx_ready;RdpgfxServerContext* gfx;} Fixture;
static BOOL active(freerdp_peer* peer){((Fixture*)peer->ContextExtra)->activated=TRUE;return TRUE;}
static BOOL mouse(rdpInput* input,UINT16 flags,UINT16 x,UINT16 y){FILE* log=((Fixture*)input->context->peer->ContextExtra)->log;fprintf(log,"M:%u:%u:%u\n",flags,x,y);fflush(log);return TRUE;}
static BOOL keyboard(rdpInput* input,UINT16 flags,UINT8 code){FILE* log=((Fixture*)input->context->peer->ContextExtra)->log;fprintf(log,"K:%u:%u\n",flags,code);fflush(log);return TRUE;}
static BOOL unicode_key(rdpInput* input,UINT16 flags,UINT16 code){FILE* log=((Fixture*)input->context->peer->ContextExtra)->log;fprintf(log,"U:%u:%u\n",flags,code);fflush(log);return TRUE;}
static BOOL paint(freerdp_peer* peer){
    BYTE pixels[32*32*4];
    for(size_t offset=0;offset<sizeof(pixels);offset+=4){pixels[offset]=0x11;pixels[offset+1]=0x22;pixels[offset+2]=0xCC;pixels[offset+3]=0;}
    BITMAP_DATA rectangle={0};
    rectangle.destRight=31;rectangle.destBottom=31;rectangle.width=32;rectangle.height=32;rectangle.bitsPerPixel=32;
    rectangle.bitmapLength=sizeof(pixels);rectangle.bitmapDataStream=pixels;rectangle.compressed=FALSE;
    BITMAP_UPDATE update={0};update.number=1;update.rectangles=&rectangle;update.skipCompression=TRUE;
    return peer->context->update->BitmapUpdate(peer->context,&update);
}
static UINT gfx_caps(RdpgfxServerContext* gfx,const RDPGFX_CAPS_ADVERTISE_PDU* caps){
    fprintf(stderr,"GFX caps received count=%u\n",caps->capsSetCount);
    if(!caps->capsSetCount)return ERROR_INVALID_DATA;
    RDPGFX_CAPS_CONFIRM_PDU confirm={.capsSet=&caps->capsSets[0]};
    UINT rc=gfx->CapsConfirm(gfx,&confirm);if(rc)return rc;
    MONITOR_DEF monitor={.left=0,.top=0,.right=639,.bottom=479,.flags=1};
    RDPGFX_RESET_GRAPHICS_PDU reset={.width=640,.height=480,.monitorCount=1,.monitorDefArray=&monitor};
    rc=gfx->ResetGraphics(gfx,&reset);if(rc)return rc;
    RDPGFX_CREATE_SURFACE_PDU surface={.surfaceId=1,.width=640,.height=480,.pixelFormat=GFX_PIXEL_FORMAT_XRGB_8888};
    rc=gfx->CreateSurface(gfx,&surface);if(rc)return rc;
    RDPGFX_MAP_SURFACE_TO_OUTPUT_PDU map={.surfaceId=1};
    rc=gfx->MapSurfaceToOutput(gfx,&map);if(rc)return rc;
    ((Fixture*)gfx->custom)->gfx_ready=TRUE;return 0;
}
static BOOL gfx_paint(Fixture* fixture){
    RdpgfxServerContext* gfx=fixture->gfx;
    RDPGFX_START_FRAME_PDU start={.frameId=1};
    if(gfx->StartFrame(gfx,&start))return FALSE;
    RECTANGLE_16 rect={0,0,640,480};
    RDPGFX_SOLID_FILL_PDU fill={.surfaceId=1,.fillRectCount=1,.fillRects=&rect};
    fill.fillPixel.B=0x11;fill.fillPixel.G=0x22;fill.fillPixel.R=0xCC;fill.fillPixel.XA=255;
    if(gfx->SolidFill(gfx,&fill))return FALSE;
    fprintf(fixture->log,"GFX_BEGIN\n");fflush(fixture->log);
    fixture->end_at=GetTickCount64()+200;return TRUE;
}
int main(int argc,char** argv){
    if(argc!=8)return 2;
    if(!winpr_InitializeSSL(WINPR_SSL_INIT_DEFAULT | WINPR_SSL_INIT_ENABLE_LOCKING))return 2;
    if(!WTSRegisterWtsApiFunctionTable(FreeRDP_InitWtsApi()))return 2;
    int port=atoi(argv[1]);if(port<1024||port>65535)return 2;
    FILE* log=fopen(argv[4],"w");if(!log)return 3;
    char sam_path[4096];snprintf(sam_path,sizeof(sam_path),"%s.sam",argv[4]);
    BYTE hash[16];if(!NTOWFv1A("fixture-password",strlen("fixture-password"),hash))return 3;
    FILE* sam=fopen(sam_path,"w");if(!sam)return 3;fputs("fixture:::",sam);for(size_t i=0;i<16;i++)fprintf(sam,"%02X",hash[i]);fputs(":\n",sam);fclose(sam);
    int listener=socket(AF_INET,SOCK_STREAM,0);if(listener<0)return 4;
    int yes=1;setsockopt(listener,SOL_SOCKET,SO_REUSEADDR,&yes,sizeof(yes));
    struct sockaddr_in address={0};address.sin_family=AF_INET;address.sin_port=htons((uint16_t)port);address.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    if(bind(listener,(struct sockaddr*)&address,sizeof(address))<0||listen(listener,8)<0)return 5;
    FILE* ready=fopen(argv[5],"w");if(!ready)return 6;fputs("ready",ready);fclose(ready);
    for(int attempt=0;attempt<8;attempt++){
        int fd=accept(listener,NULL,NULL);if(fd<0)break;
        Fixture fixture={.log=log};freerdp_peer* peer=freerdp_peer_new(fd);if(!peer){close(fd);break;}
        peer->ContextExtra=&fixture;peer->ContextSize=sizeof(rdpContext);
        if(!freerdp_peer_context_new(peer)){freerdp_peer_free(peer);continue;}
        HANDLE vcm=attempt==7?WTSOpenServerA((LPSTR)peer->context):NULL;
        if(attempt==7&&(!vcm||vcm==INVALID_HANDLE_VALUE))return 7;
        rdpSettings* settings=peer->context->settings;
        if(attempt==7&&!freerdp_settings_set_bool(settings,FreeRDP_SupportGraphicsPipeline,TRUE))return 7;
        rdpPrivateKey* key=freerdp_key_new_from_file(argv[attempt==6?7:3]);rdpCertificate* cert=freerdp_certificate_new_from_file(argv[attempt==6?6:2]);
        BOOL ok=key&&cert&&freerdp_settings_set_pointer_len(settings,FreeRDP_RdpServerRsaKey,key,1)
            &&freerdp_settings_set_pointer_len(settings,FreeRDP_RdpServerCertificate,cert,1)
            &&freerdp_settings_set_bool(settings,FreeRDP_TlsSecurity,TRUE)
            &&freerdp_settings_set_bool(settings,FreeRDP_NlaSecurity,attempt>0)
            &&freerdp_settings_set_string(settings,FreeRDP_NtlmSamFile,sam_path)
            &&freerdp_settings_set_bool(settings,FreeRDP_ExtSecurity,FALSE)
            &&freerdp_settings_set_bool(settings,FreeRDP_RdpSecurity,FALSE)
            &&freerdp_settings_set_uint32(settings,FreeRDP_ColorDepth,32);
        peer->PostConnect=active;peer->Activate=active;
        peer->context->input->MouseEvent=mouse;peer->context->input->KeyboardEvent=keyboard;peer->context->input->UnicodeKeyboardEvent=unicode_key;
        if(ok)ok=peer->Initialize(peer);
        UINT64 deadline=GetTickCount64()+20000;int previous_state=-1;
        while(ok&&GetTickCount64()<deadline){
            HANDLE handles[64]={0};DWORD count=peer->GetEventHandles(peer,handles,60);if(!count)break;
            if(vcm)handles[count++]=WTSVirtualChannelManagerGetEventHandle(vcm);
            HANDLE gfx_event=fixture.gfx?rdpgfx_server_get_event_handle(fixture.gfx):NULL;
            if(gfx_event)handles[count++]=gfx_event;
            DWORD status=WaitForMultipleObjects(count,handles,FALSE,50);if(status==WAIT_FAILED)break;
            if(status!=WAIT_TIMEOUT&&!peer->CheckFileDescriptor(peer))break;
            if(vcm){
                /* Opening before activation permanently misses the unjoined drdynvc. */
                if(!WTSVirtualChannelManagerCheckFileDescriptorEx(vcm,fixture.activated)){fprintf(stderr,"GFX vcm check failed\n");break;}
                int state=WTSVirtualChannelManagerGetDrdynvcState(vcm)+(fixture.activated?100:0);
                if(state!=previous_state){fprintf(stderr,"GFX state=%d activated=%d joined=%d\n",state,fixture.activated,WTSVirtualChannelManagerIsChannelJoined(vcm,"drdynvc"));previous_state=state;}
                if(fixture.activated&&!fixture.gfx&&WTSVirtualChannelManagerGetDrdynvcState(vcm)==DRDYNVC_STATE_READY){
                    fprintf(stderr,"GFX opening\n");
                    fixture.gfx=rdpgfx_server_context_new(vcm);if(!fixture.gfx)break;
                    fixture.gfx->rdpcontext=peer->context;fixture.gfx->custom=&fixture;fixture.gfx->CapsAdvertise=gfx_caps;
                    if(!fixture.gfx->Initialize(fixture.gfx,TRUE)||!fixture.gfx->Open(fixture.gfx))break;
                }
                if(gfx_event&&WaitForSingleObject(gfx_event,0)==WAIT_OBJECT_0&&rdpgfx_server_handle_messages(fixture.gfx))break;
                if(fixture.gfx_ready&&!fixture.painted){fixture.painted=TRUE;if(!gfx_paint(&fixture))break;}
            }
            if(attempt!=7&&fixture.activated&&!fixture.painted){
                fixture.painted=TRUE;
                SURFACE_FRAME_MARKER begin={.frameAction=SURFACECMD_FRAMEACTION_BEGIN,.frameId=1};
                fprintf(log,"FRAME_BEGIN\n");fflush(log);
                if(!peer->context->update->SurfaceFrameMarker(peer->context,&begin)||!paint(peer))break;
                fixture.end_at=GetTickCount64()+200;
            }
            if(fixture.end_at&&GetTickCount64()>=fixture.end_at){
                if(attempt==7){
                    RDPGFX_END_FRAME_PDU end={.frameId=1};
                    fprintf(log,"GFX_END\n");fflush(log);
                    if(fixture.gfx->EndFrame(fixture.gfx,&end))break;
                    fixture.end_at=0;continue;
                }
                SURFACE_FRAME_MARKER end={.frameAction=SURFACECMD_FRAMEACTION_END,.frameId=1};
                fprintf(log,"FRAME_END\n");fflush(log);
                if(!peer->context->update->SurfaceFrameMarker(peer->context,&end))break;
                fixture.end_at=0;
            }
        }
        if(fixture.gfx){fixture.gfx->Close(fixture.gfx);rdpgfx_server_context_free(fixture.gfx);}
        if(vcm)WTSCloseServer(vcm);
        peer->Disconnect(peer);freerdp_peer_context_free(peer);freerdp_peer_free(peer);
    }
    close(listener);fclose(log);return 0;
}

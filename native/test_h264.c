/* Decode a generated red frame. No desktop capture, credentials or network. */
#include <freerdp/codec/h264.h>
#include <freerdp/codec/color.h>
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
int main(int argc,char** argv){
    assert(argc==3);
    FILE* file=fopen(argv[1],"rb");assert(file);
    assert(fseek(file,0,SEEK_END)==0);long size=ftell(file);assert(size>0&&size<65536);
    rewind(file);BYTE* data=calloc(1,(size_t)size+64);assert(data);
    assert(fread(data,1,(size_t)size,file)==(size_t)size);fclose(file);
    H264_CONTEXT* decoder=h264_context_new(FALSE);assert(decoder);
    BOOL hw=atoi(argv[2])!=0;
    assert(h264_context_set_option(decoder,H264_CONTEXT_OPTION_HW_ACCEL,hw));
    assert(h264_context_reset(decoder,64,64));
    BYTE output[64*64*4]={0};RECTANGLE_16 rect={0,0,64,64};
    INT32 rc=avc420_decompress(decoder,data,(UINT32)size,output,PIXEL_FORMAT_BGRX32,64*4,64,64,&rect,1);
    assert(rc>=0);
    for(size_t i=0;i<sizeof(output);i+=4)assert(output[i]<70&&output[i+1]<70&&output[i+2]>180);
    printf("H264 red-frame decode OK (hardware requested=%d)\n",hw);
    h264_context_free(decoder);free(data);return 0;
}

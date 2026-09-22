/*
 * camtest.c — DirectShow camera probe for testing wine's webcam stack.
 *
 * 1. Enumerates DirectShow video input devices (wine qcap/devenum) and
 *    prints each device's IAMStreamConfig caps (resolution, subtype GUID).
 * 2. Builds a capture graph (device -> SampleGrabber -> Null Renderer),
 *    runs it, grabs one frame and saves it as a 24-bit BMP — the exact
 *    capture path ZaloCall uses under wine.
 *
 * Build:  i686-w64-mingw32-gcc camtest.c -lstrmiids -lole32 -loleaut32 -o camtest.exe
 * Run:    WINEPREFIX=... wine camtest.exe [out.bmp]
 *         (optionally with LD_PRELOAD=streamproxy-x86_64.so to include the
 *          shim, exactly as the app runs it)
 *
 * Exit codes (used by the plugin's camera probe):
 *   0 = a frame was captured — wine's native camera path WORKS
 *   1 = no camera device visible to DirectShow (nothing to bridge)
 *   2-6 = DirectShow setup failures (the bridge cannot help either)
 *   7 = capture graph ran but no frame arrived (the EPIPE race family)
 *   crash/hang = the wine qcap crash the bridge exists to work around
 */
#include <windows.h>
#include <dshow.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const CLSID CLSID_SampleGrabber = {0xC1F400A0,0x3F08,0x11D3,{0x9F,0x0B,0x00,0x60,0x08,0x03,0x9E,0x37}};
static const CLSID CLSID_NullRenderer = {0xC1F400A4,0x3F08,0x11D3,{0x9F,0x0B,0x00,0x60,0x08,0x03,0x9E,0x37}};
static const IID IID_ISampleGrabber = {0x6B652FFF,0x11FE,0x4FCE,{0x92,0xAD,0x02,0x66,0xB5,0xD7,0xC7,0x8F}};

/* ISampleGrabber vtable (qedit.h layout, declared manually for C) */
typedef struct ISampleGrabber ISampleGrabber;
struct ISampleGrabberVtbl {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(ISampleGrabber *, REFIID, void **);
    ULONG   (STDMETHODCALLTYPE *AddRef)(ISampleGrabber *);
    ULONG   (STDMETHODCALLTYPE *Release)(ISampleGrabber *);
    HRESULT (STDMETHODCALLTYPE *SetOneShot)(ISampleGrabber *, BOOL);
    HRESULT (STDMETHODCALLTYPE *SetMediaType)(ISampleGrabber *, const AM_MEDIA_TYPE *);
    HRESULT (STDMETHODCALLTYPE *GetConnectedMediaType)(ISampleGrabber *, AM_MEDIA_TYPE *);
    HRESULT (STDMETHODCALLTYPE *SetBufferSamples)(ISampleGrabber *, BOOL);
    HRESULT (STDMETHODCALLTYPE *GetCurrentBuffer)(ISampleGrabber *, long *, long *);
    HRESULT (STDMETHODCALLTYPE *SetCallback)(ISampleGrabber *, void *, long);
};
struct ISampleGrabber { struct ISampleGrabberVtbl *lpVtbl; };

static void *qi(IUnknown *u, REFIID iid) {
    void *p = NULL;
    if (u) u->lpVtbl->QueryInterface(u, iid, &p);
    return p;
}

static IPin *find_pin(IBaseFilter *f, int want_output) {
    IEnumPins *ep = NULL;
    IPin *found = NULL;
    if (f->lpVtbl->EnumPins(f, &ep) == S_OK) {
        IPin *pin = NULL;
        while (ep->lpVtbl->Next(ep, 1, &pin, NULL) == S_OK) {
            PIN_DIRECTION d;
            pin->lpVtbl->QueryDirection(pin, &d);
            if ((want_output && d == PINDIR_OUTPUT) || (!want_output && d == PINDIR_INPUT)) {
                found = pin;
                break;
            }
            pin->lpVtbl->Release(pin);
        }
        ep->lpVtbl->Release(ep);
    }
    return found;
}

static IBaseFilter *enumerate(void) {
    ICreateDevEnum *de = NULL;
    IBaseFilter *first = NULL;
    if (CoCreateInstance(&CLSID_SystemDeviceEnum, NULL, CLSCTX_INPROC,
                         &IID_ICreateDevEnum, (void **)&de) != S_OK) {
        printf("SystemDeviceEnum failed\n");
        return NULL;
    }
    IEnumMoniker *em = NULL;
    if (de->lpVtbl->CreateClassEnumerator(de, &CLSID_VideoInputDeviceCategory, &em, 0) == S_OK) {
        IMoniker *m = NULL;
        int idx = 0;
        while (em->lpVtbl->Next(em, 1, &m, NULL) == S_OK) {
            IPropertyBag *pb = NULL;
            VARIANT v;
            VariantInit(&v);
            m->lpVtbl->BindToStorage(m, 0, 0, &IID_IPropertyBag, (void **)&pb);
            if (pb) {
                pb->lpVtbl->Read(pb, L"FriendlyName", &v, 0);
                printf("device %d: %ls\n", idx, v.bstrVal ? v.bstrVal : L"?");
            }
            VariantClear(&v);
            if (pb) pb->lpVtbl->Release(pb);
            IBaseFilter *f = NULL;
            if (m->lpVtbl->BindToObject(m, 0, 0, &IID_IBaseFilter, (void **)&f) == S_OK && f) {
                IEnumPins *ep = NULL;
                f->lpVtbl->EnumPins(f, &ep);
                IPin *pin = NULL;
                while (ep && ep->lpVtbl->Next(ep, 1, &pin, NULL) == S_OK) {
                    IAMStreamConfig *sc = (IAMStreamConfig *)qi((IUnknown *)pin, &IID_IAMStreamConfig);
                    if (sc) {
                        int n = 0, sz = 0;
                        sc->lpVtbl->GetNumberOfCapabilities(sc, &n, &sz);
                        printf("  caps: %d (struct %d bytes)\n", n, sz);
                        for (int i = 0; i < n && i < 5; i++) {
                            AM_MEDIA_TYPE *pmt = NULL;
                            BYTE buf[512];
                            if (sc->lpVtbl->GetStreamCaps(sc, i, &pmt, buf) == S_OK && pmt) {
                                VIDEOINFOHEADER *vh = (VIDEOINFOHEADER *)pmt->pbFormat;
                                printf("    [%d] %dx%d subtype %08lx\n", i,
                                       vh->bmiHeader.biWidth, vh->bmiHeader.biHeight,
                                       pmt->subtype.Data1);
                            }
                        }
                        sc->lpVtbl->Release(sc);
                    }
                    pin->lpVtbl->Release(pin);
                }
                if (ep) ep->lpVtbl->Release(ep);
                if (!first) first = f;
                else f->lpVtbl->Release(f);
            }
            m->lpVtbl->Release(m);
            idx++;
        }
        em->lpVtbl->Release(em);
    }
    de->lpVtbl->Release(de);
    return first;
}

static int save_bmp(const char *path, const BITMAPINFOHEADER *bi, const void *pixels) {
    FILE *f = fopen(path, "wb");
    if (!f) return 0;
    int rowsize = ((bi->biWidth * bi->biBitCount + 31) / 32) * 4;
    int imgsize = rowsize * bi->biHeight;
    BITMAPFILEHEADER fh;
    memset(&fh, 0, sizeof(fh));
    fh.bfType = 0x4D42;
    fh.bfSize = sizeof(fh) + sizeof(BITMAPINFOHEADER) + imgsize;
    fh.bfOffBits = sizeof(fh) + sizeof(BITMAPINFOHEADER);
    fwrite(&fh, sizeof(fh), 1, f);
    fwrite(bi, sizeof(BITMAPINFOHEADER), 1, f);
    const BYTE *p = (const BYTE *)pixels;
    for (int y = bi->biHeight - 1; y >= 0; y--) fwrite(p + y * rowsize, 1, rowsize, f);
    fclose(f);
    return 1;
}

int main(int argc, char **argv) {
    const char *out = (argc > 1) ? argv[1] : "camtest.bmp";
    CoInitialize(NULL);

    IBaseFilter *cam = enumerate();
    if (!cam) {
        printf("NO CAMERA DEVICE visible to DirectShow\n");
        CoUninitialize();
        return 1;
    }

    printf("--- capture graph: camera -> SampleGrabber -> NullRenderer ---\n");
    IGraphBuilder *g = NULL;
    IBaseFilter *sg = NULL, *nr = NULL;
    if (CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC,
                         &IID_IGraphBuilder, (void **)&g) != S_OK) { printf("FilterGraph failed\n"); return 2; }
    if (CoCreateInstance(&CLSID_SampleGrabber, NULL, CLSCTX_INPROC,
                         &IID_IBaseFilter, (void **)&sg) != S_OK) { printf("SampleGrabber create failed\n"); return 3; }
    if (CoCreateInstance(&CLSID_NullRenderer, NULL, CLSCTX_INPROC,
                         &IID_IBaseFilter, (void **)&nr) != S_OK) { printf("NullRenderer create failed\n"); return 4; }
    g->lpVtbl->AddFilter(g, cam, L"cam");
    g->lpVtbl->AddFilter(g, sg, L"grab");
    g->lpVtbl->AddFilter(g, nr, L"null");

    IPin *cam_out = find_pin(cam, 1), *sg_in = find_pin(sg, 0);
    IPin *sg_out = find_pin(sg, 1), *nr_in = find_pin(nr, 0);
    if (!cam_out || !sg_in) { printf("pin lookup failed (cam_out=%p sg_in=%p)\n", (void*)cam_out, (void*)sg_in); return 5; }
    HRESULT hr = g->lpVtbl->Connect(g, cam_out, sg_in);
    printf("connect cam->grab: 0x%08lx\n", hr);
    if (hr == S_OK && sg_out && nr_in) {
        hr = g->lpVtbl->Connect(g, sg_out, nr_in);
        printf("connect grab->null: 0x%08lx\n", hr);
    }

    ISampleGrabber *grab = (ISampleGrabber *)qi((IUnknown *)sg, &IID_ISampleGrabber);
    if (!grab) { printf("no ISampleGrabber\n"); return 6; }
    grab->lpVtbl->SetBufferSamples(grab, TRUE);
    grab->lpVtbl->SetOneShot(grab, FALSE);

    IMediaControl *mc = (IMediaControl *)qi((IUnknown *)g, &IID_IMediaControl);
    hr = mc->lpVtbl->Run(mc);
    printf("Run: 0x%08lx\n", hr);
    Sleep(3000);

    AM_MEDIA_TYPE mt;
    memset(&mt, 0, sizeof(mt));
    if (grab->lpVtbl->GetConnectedMediaType(grab, &mt) == S_OK && mt.pbFormat) {
        VIDEOINFOHEADER *vh = (VIDEOINFOHEADER *)mt.pbFormat;
        printf("negotiated: %dx%d bitcount=%d sizeimage=%d\n",
               vh->bmiHeader.biWidth, vh->bmiHeader.biHeight,
               vh->bmiHeader.biBitCount, vh->bmiHeader.biSizeImage);
        long size = vh->bmiHeader.biSizeImage;
        if (size <= 0) size = vh->bmiHeader.biWidth * vh->bmiHeader.biHeight * 3;
        void *buf = malloc(size);
        hr = grab->lpVtbl->GetCurrentBuffer(grab, &size, (long *)buf);
        printf("GetCurrentBuffer: 0x%08lx (size %ld)\n", hr, size);
        if (hr == S_OK && size > 0) {
            /* crop header to what the buffer holds (top-down vs bottom-up) */
            if (save_bmp(out, &vh->bmiHeader, buf)) printf("saved %s\n", out);
            else printf("BMP write failed\n");
            free(buf);
            return 0; /* PROBE: frame captured — native path works */
        }
        free(buf);
    } else {
        printf("GetConnectedMediaType failed\n");
    }
    /* Graph ran but no frame arrived: the V4L2/DQBUF failure family. */
    return 7;
}

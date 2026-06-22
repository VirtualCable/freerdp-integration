// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

// Authors: Adolfo Gómez, dkmaster at dkmon dot com
#include <freerdp/freerdp.h>
#include <freerdp/client.h>
#include <freerdp/pointer.h>
#include <freerdp/rail.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/codec/color.h>
#include <freerdp/transport_io.h>

// Client
#include <freerdp/client/disp.h>
#include <freerdp/client/cliprdr.h>
#include <freerdp/client/rdpsnd.h>
#include <freerdp/client/audin.h>
#include <freerdp/client/rail.h>
#include <freerdp/client/rdpgfx.h>
#include <freerdp/client/cmdline.h>

// Channels
#include <freerdp/channels/channels.h>
#include <freerdp/channels/disp.h>
#include <freerdp/channels/cliprdr.h>
#include <freerdp/channels/rail.h>
#include <freerdp/channels/rdpsnd.h>
#include <freerdp/channels/audin.h>
#include <freerdp/channels/rdpecam.h>
#include <freerdp/channels/rdpdr.h>
#include <freerdp/channels/scard.h>
#include <freerdp/utils/smartcard_call.h>
#include <freerdp/utils/smartcard_operations.h>
#include <freerdp/utils/smartcard_pack.h>

#include <freerdp/gdi/gfx.h>
#include <freerdp/codec/h264.h>

#include <winpr/synch.h>
#include <winpr/wtypes.h>
#include <winpr/winpr.h>
#include <winpr/wlog.h>

// BSD 3-Clause License
// Copyright (c) 2026, Virtual Cable S.L.
// All rights reserved.
// Authors: Adolfo Gómez, dkmaster at dkmon dot com

//! IOCTL dispatch and handler functions for smartcard IRPs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::integrations::SmartcardIntegration;
use crate::integrations::smartcard::{
    SCARD_E_INVALID_HANDLE, SCARD_E_NO_READERS_AVAILABLE, SCARD_E_UNSUPPORTED_FEATURE,
    SCARD_S_SUCCESS, SCARD_STATE_ATRMATCH,
};
use crate::utils::log;

use super::consts::*;
use super::device::{
    ContextEntry, build_multi_string_ascii, build_multi_string_utf16, get_atr_masks,
    get_card_handle, get_io_request, get_reader_states_in_a, get_reader_states_in_w,
    pack_reader_states_out, u16_ptr_to_string,
};

// ---------------------------------------------------------------------------
// IOCTL Dispatcher
// ---------------------------------------------------------------------------

pub(crate) fn dispatch_ioctl(
    ioctl: u32,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    match ioctl {
        SCARD_IOCTL_ESTABLISHCONTEXT => {
            handle_establish_context(integration, contexts, operation, out)
        }
        SCARD_IOCTL_RELEASECONTEXT => handle_release_context(integration, contexts, operation),
        SCARD_IOCTL_ISVALIDCONTEXT => handle_is_valid_context(contexts, operation),
        SCARD_IOCTL_CANCEL => handle_cancel(integration, contexts, operation),
        SCARD_IOCTL_ACCESSSTARTEDEVENT => handle_access_started_event(operation),
        SCARD_IOCTL_RELEASETARTEDEVENT => handle_release_started_event(operation),
        SCARD_IOCTL_LISTREADERGROUPSA | SCARD_IOCTL_LISTREADERSA => {
            handle_list_readers(integration, contexts, operation, out, false)
        }
        SCARD_IOCTL_LISTREADERGROUPSW | SCARD_IOCTL_LISTREADERSW => {
            handle_list_readers(integration, contexts, operation, out, true)
        }
        SCARD_IOCTL_CONNECTA => handle_connect(integration, contexts, operation, out, false),
        SCARD_IOCTL_CONNECTW => handle_connect(integration, contexts, operation, out, true),
        SCARD_IOCTL_RECONNECT => handle_reconnect(integration, operation, out),
        SCARD_IOCTL_DISCONNECT => handle_disconnect(integration, operation),
        SCARD_IOCTL_BEGINTRANSACTION => handle_begin_transaction(integration, operation),
        SCARD_IOCTL_ENDTRANSACTION => handle_end_transaction(integration, operation),
        SCARD_IOCTL_TRANSMIT => handle_transmit(integration, operation, out),
        SCARD_IOCTL_CONTROL => handle_control(integration, operation, out),
        SCARD_IOCTL_GETATTRIB => handle_get_attrib(integration, operation, out),
        SCARD_IOCTL_SETATTRIB => handle_set_attrib(integration, operation),
        SCARD_IOCTL_STATE => handle_state(integration, operation, out),
        SCARD_IOCTL_STATUSA => handle_status(integration, operation, out, false),
        SCARD_IOCTL_STATUSW => handle_status(integration, operation, out, true),
        SCARD_IOCTL_GETSTATUSCHANGEA => {
            handle_get_status_change(integration, contexts, operation, out, false)
        }
        SCARD_IOCTL_GETSTATUSCHANGEW => {
            handle_get_status_change(integration, contexts, operation, out, true)
        }
        SCARD_IOCTL_LOCATECARDSA => handle_locate_cards(operation, out, false),
        SCARD_IOCTL_LOCATECARDSW => handle_locate_cards(operation, out, true),
        SCARD_IOCTL_LOCATECARDSBYATRA => {
            handle_locate_cards_by_atr(integration, contexts, operation, out, false)
        }
        SCARD_IOCTL_LOCATECARDSBYATRW => {
            handle_locate_cards_by_atr(integration, contexts, operation, out, true)
        }
        SCARD_IOCTL_GETTRANSMITCOUNT => handle_get_transmit_count(operation, out),
        SCARD_IOCTL_GETDEVICETYPEID => handle_get_device_type_id(operation, out),
        SCARD_IOCTL_READCACHEA | SCARD_IOCTL_READCACHEW => {
            handle_read_cache(operation, out, ioctl == SCARD_IOCTL_READCACHEW)
        }
        SCARD_IOCTL_WRITECACHEA | SCARD_IOCTL_WRITECACHEW => {
            handle_write_cache(operation, ioctl == SCARD_IOCTL_WRITECACHEW)
        }
        SCARD_IOCTL_GETREADERICON => SCARD_S_SUCCESS,
        _ => SCARD_E_UNSUPPORTED_FEATURE,
    }
}

// ---------------------------------------------------------------------------
// IOCTL Handlers
// ---------------------------------------------------------------------------

fn handle_establish_context(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let scope = unsafe { operation.call.establishContext.dwScope };
    log::debug!("smartcard: ESTABLISH_CONTEXT scope={}", scope);

    match integration.establish_context(scope) {
        Ok(ctx) => {
            let ctx_id = ctx.raw();
            log::info!("smartcard: established context 0x{:X}", ctx_id);

            let mut ctx_map = contexts.lock().unwrap();
            ctx_map.insert(ctx_id, ContextEntry { context: ctx });

            unsafe {
                let mut pb_context = [0u8; 8];
                pb_context[..8].copy_from_slice(&ctx_id.to_le_bytes());
                let ret = freerdp_sys::EstablishContext_Return {
                    ReturnCode: SCARD_S_SUCCESS as i32,
                    hContext: freerdp_sys::REDIR_SCARDCONTEXT {
                        cbContext: 8,
                        pbContext: pb_context,
                    },
                };
                freerdp_sys::smartcard_pack_establish_context_return(out, &ret);
            }

            SCARD_S_SUCCESS
        }
        Err(code) => {
            log::error!("smartcard: establish_context failed: 0x{:08X}", code);
            code
        }
    }
}

fn handle_release_context(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    log::debug!("smartcard: RELEASE_CONTEXT 0x{:X}", ctx_id);

    let mut ctx_map = contexts.lock().unwrap();
    if let Some(entry) = ctx_map.remove(&ctx_id) {
        match integration.release_context(&entry.context) {
            Ok(()) => {
                log::info!("smartcard: released context 0x{:X}", ctx_id);
                SCARD_S_SUCCESS
            }
            Err(code) => {
                log::error!(
                    "smartcard: release_context failed for 0x{:X}: 0x{:08X}",
                    ctx_id,
                    code
                );
                code
            }
        }
    } else {
        log::warn!(
            "smartcard: RELEASE_CONTEXT for unknown context 0x{:X}",
            ctx_id
        );
        SCARD_E_INVALID_HANDLE
    }
}

fn handle_is_valid_context(
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    let ctx_map = contexts.lock().unwrap();
    if ctx_map.contains_key(&ctx_id) {
        SCARD_S_SUCCESS
    } else {
        SCARD_E_INVALID_HANDLE
    }
}

fn handle_cancel(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    log::debug!("smartcard: CANCEL for context 0x{:X}", ctx_id);

    let ctx_map = contexts.lock().unwrap();
    if let Some(entry) = ctx_map.get(&ctx_id) {
        match integration.cancel(&entry.context) {
            Ok(()) => SCARD_S_SUCCESS,
            Err(code) => code,
        }
    } else {
        SCARD_E_INVALID_HANDLE
    }
}

fn handle_access_started_event(_operation: &freerdp_sys::SMARTCARD_OPERATION) -> u32 {
    log::debug!("smartcard: ACCESS_STARTED_EVENT — responding success");
    SCARD_S_SUCCESS
}

fn handle_release_started_event(_operation: &freerdp_sys::SMARTCARD_OPERATION) -> u32 {
    log::debug!("smartcard: RELEASE_STARTED_EVENT — responding success");
    SCARD_S_SUCCESS
}

fn handle_list_readers(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    let ioctl = operation.ioControlCode;
    log::debug!("smartcard: LIST_READERS/GROUPS for context 0x{:X}", ctx_id);

    let ctx = {
        let ctx_map = contexts.lock().unwrap();
        match ctx_map.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    match integration.list_readers(&ctx, None) {
        Ok(readers) => {
            log::debug!(
                "smartcard: found {} reader(s): {:?}",
                readers.len(),
                readers
            );
            if readers.is_empty() {
                return SCARD_E_NO_READERS_AVAILABLE;
            }

            let mut msz_bytes = if unicode {
                build_multi_string_utf16(&readers)
            } else {
                build_multi_string_ascii(&readers)
            };

            let ret = freerdp_sys::ListReaders_Return {
                ReturnCode: SCARD_S_SUCCESS as i32,
                cBytes: msz_bytes.len() as u32,
                msz: msz_bytes.as_mut_ptr(),
            };

            unsafe {
                if ioctl == SCARD_IOCTL_LISTREADERGROUPSA || ioctl == SCARD_IOCTL_LISTREADERGROUPSW
                {
                    freerdp_sys::smartcard_pack_list_reader_groups_return(
                        out,
                        &ret,
                        if unicode { 1 } else { 0 },
                    );
                } else {
                    freerdp_sys::smartcard_pack_list_readers_return(
                        out,
                        &ret,
                        if unicode { 1 } else { 0 },
                    );
                }
            }

            SCARD_S_SUCCESS
        }
        Err(code) => {
            log::debug!("smartcard: list_readers failed: 0x{:08X}", code);
            code
        }
    }
}

fn handle_connect(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    log::debug!("smartcard: CONNECT for context 0x{:X}", ctx_id);

    let ctx = {
        let ctx_map = contexts.lock().unwrap();
        match ctx_map.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    let (reader_name, share_mode, preferred_protocols) = unsafe {
        if unicode {
            let call = &operation.call.connectW;
            (
                u16_ptr_to_string(call.szReader),
                call.Common.dwShareMode,
                call.Common.dwPreferredProtocols,
            )
        } else {
            let call = &operation.call.connectA;
            let name = if call.szReader.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(call.szReader)
                    .to_string_lossy()
                    .into_owned()
            };
            (
                name,
                call.Common.dwShareMode,
                call.Common.dwPreferredProtocols,
            )
        }
    };

    match integration.connect(&ctx, &reader_name, share_mode, preferred_protocols) {
        Ok(result) => {
            log::info!(
                "smartcard: connected to '{}', handle=0x{:X}, protocol=0x{:X}",
                reader_name,
                result.handle.raw(),
                result.active_protocol
            );

            unsafe {
                let mut pb_context = [0u8; 8];
                pb_context[..8].copy_from_slice(&ctx_id.to_le_bytes());
                let mut pb_handle = [0u8; 8];
                pb_handle[..8].copy_from_slice(&result.handle.raw().to_le_bytes());

                let ret = freerdp_sys::Connect_Return {
                    ReturnCode: SCARD_S_SUCCESS as i32,
                    hContext: freerdp_sys::REDIR_SCARDCONTEXT {
                        cbContext: 8,
                        pbContext: pb_context,
                    },
                    hCard: freerdp_sys::REDIR_SCARDHANDLE {
                        cbHandle: 8,
                        pbHandle: pb_handle,
                    },
                    dwActiveProtocol: result.active_protocol,
                };

                freerdp_sys::smartcard_pack_connect_return(out, &ret);
            }

            SCARD_S_SUCCESS
        }
        Err(code) => {
            log::debug!("smartcard: connect failed: 0x{:08X}", code);
            code
        }
    }
}

fn handle_reconnect(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.reconnect };
    let card_handle = get_card_handle(&call.handles.hCard);
    let share_mode = call.dwShareMode;
    let preferred_protocols = call.dwPreferredProtocols;
    let initialization = call.dwInitialization;
    log::debug!(
        "smartcard: RECONNECT card=0x{:X} share_mode={} preferred_protocols={} initialization={}",
        card_handle.raw(),
        share_mode,
        preferred_protocols,
        initialization
    );

    match integration.reconnect(
        &card_handle,
        share_mode,
        preferred_protocols,
        initialization,
    ) {
        Ok(active_protocol) => {
            unsafe {
                let ret = freerdp_sys::Reconnect_Return {
                    ReturnCode: SCARD_S_SUCCESS as i32,
                    dwActiveProtocol: active_protocol,
                };
                freerdp_sys::smartcard_pack_reconnect_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_disconnect(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.hCardAndDisposition };
    let card_handle = get_card_handle(&call.handles.hCard);
    let disposition = call.dwDisposition;
    log::debug!(
        "smartcard: DISCONNECT card=0x{:X} disposition={}",
        card_handle.raw(),
        disposition
    );

    match integration.disconnect(&card_handle, disposition) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_begin_transaction(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.hCardAndDisposition };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!(
        "smartcard: BEGIN_TRANSACTION card=0x{:X}",
        card_handle.raw()
    );

    match integration.begin_transaction(&card_handle) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_end_transaction(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.hCardAndDisposition };
    let card_handle = get_card_handle(&call.handles.hCard);
    let disposition = call.dwDisposition;
    log::debug!(
        "smartcard: END_TRANSACTION card=0x{:X} disposition={}",
        card_handle.raw(),
        disposition
    );

    match integration.end_transaction(&card_handle, disposition) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_state(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.state };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!("smartcard: STATE card=0x{:X}", card_handle.raw());

    match integration.status(&card_handle) {
        Ok(status_info) => {
            let mut rg_atr = [0u8; 36];
            let atr_len = status_info.atr.len().min(36);
            rg_atr[..atr_len].copy_from_slice(&status_info.atr[..atr_len]);

            unsafe {
                let ret = freerdp_sys::State_Return {
                    ReturnCode: SCARD_S_SUCCESS as i32,
                    dwState: status_info.state,
                    dwProtocol: status_info.protocol,
                    cbAtrLen: atr_len as u32,
                    rgAtr: rg_atr,
                };
                freerdp_sys::smartcard_pack_state_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_status(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let call = unsafe { &operation.call.status };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!("smartcard: STATUS card=0x{:X}", card_handle.raw());

    match integration.status(&card_handle) {
        Ok(status_info) => {
            let mut msz_bytes = if unicode {
                build_multi_string_utf16(&status_info.reader_names)
            } else {
                build_multi_string_ascii(&status_info.reader_names)
            };

            let mut pb_atr = [0u8; 32];
            let atr_len = status_info.atr.len().min(32);
            pb_atr[..atr_len].copy_from_slice(&status_info.atr[..atr_len]);

            unsafe {
                let ret = freerdp_sys::Status_Return {
                    ReturnCode: SCARD_S_SUCCESS as i32,
                    cBytes: msz_bytes.len() as u32,
                    mszReaderNames: msz_bytes.as_mut_ptr(),
                    dwState: status_info.state,
                    dwProtocol: status_info.protocol,
                    pbAtr: pb_atr,
                    cbAtrLen: atr_len as u32,
                };
                freerdp_sys::smartcard_pack_status_return(out, &ret, if unicode { 1 } else { 0 });
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_transmit(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.transmit };
    let card_handle = get_card_handle(&call.handles.hCard);
    log::debug!(
        "smartcard: TRANSMIT card=0x{:X} send_len={}",
        card_handle.raw(),
        call.cbSendLength
    );

    let send_pci = get_io_request(call.pioSendPci);
    let send_data =
        unsafe { std::slice::from_raw_parts(call.pbSendBuffer, call.cbSendLength as usize) };

    log::debug!(
        "smartcard: TRANSMIT APDU send ({} bytes): {}",
        send_data.len(),
        send_data
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ")
    );

    match integration.transmit(&card_handle, &send_pci, send_data) {
        Ok(mut result) => {
            // Auto-handle GET RESPONSE chaining (61 XX status)
            // SCard API should handle this transparently, but through RDP redirect
            // we must do it manually so msclmd receives the complete response.
            while result.recv_buffer.len() >= 2 {
                let last = result.recv_buffer.len();
                let sw1 = result.recv_buffer[last - 2];
                let sw2 = result.recv_buffer[last - 1];
                if sw1 != 0x61 {
                    break;
                }
                // Strip 61 XX, issue GET RESPONSE for remaining bytes
                let remaining = sw2 as usize;
                result.recv_buffer.truncate(last - 2);
                let get_resp = [
                    0x00,
                    0xC0,
                    0x00,
                    0x00,
                    if remaining == 0 { 0x00 } else { sw2 },
                ];
                match integration.transmit(&card_handle, &send_pci, &get_resp) {
                    Ok(gr) => {
                        result.recv_buffer.extend_from_slice(&gr.recv_buffer);
                    }
                    Err(_) => break,
                }
            }

            log::debug!(
                "smartcard: TRANSMIT APDU recv ({} bytes): {}",
                result.recv_buffer.len(),
                result
                    .recv_buffer
                    .iter()
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" ")
            );

            let pio_recv_pci = if let Some(ref recv_pci) = result.recv_pci {
                let mut pci_buf = Vec::new();
                pci_buf.extend_from_slice(&recv_pci.protocol.to_le_bytes());
                let total_pci_len = (8 + recv_pci.extra_bytes.len()) as u32;
                pci_buf.extend_from_slice(&total_pci_len.to_le_bytes());
                pci_buf.extend_from_slice(&recv_pci.extra_bytes);
                Some(pci_buf)
            } else {
                None
            };

            let mut recv_buf = result.recv_buffer;
            let ret = freerdp_sys::Transmit_Return {
                ReturnCode: SCARD_S_SUCCESS as i32,
                pioRecvPci: pio_recv_pci
                    .as_ref()
                    .map(|b| b.as_ptr() as freerdp_sys::LPSCARD_IO_REQUEST)
                    .unwrap_or(std::ptr::null_mut()),
                cbRecvLength: recv_buf.len() as u32,
                pbRecvBuffer: recv_buf.as_mut_ptr(),
            };

            unsafe {
                freerdp_sys::smartcard_pack_transmit_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_control(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.control };
    let card_handle = get_card_handle(&call.handles.hCard);
    let control_code = call.dwControlCode;
    log::debug!(
        "smartcard: CONTROL card=0x{:X} code=0x{:X} len={}",
        card_handle.raw(),
        control_code,
        call.cbInBufferSize
    );

    let in_data = if call.pvInBuffer.is_null() || call.cbInBufferSize == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(call.pvInBuffer, call.cbInBufferSize as usize) }
    };

    match integration.control(&card_handle, control_code, in_data) {
        Ok(mut out_data) => {
            log::debug!(
                "smartcard: control success, response_len={}",
                out_data.len()
            );
            unsafe {
                let ret = freerdp_sys::Control_Return {
                    ReturnCode: SCARD_S_SUCCESS as i32,
                    cbOutBufferSize: out_data.len() as u32,
                    pvOutBuffer: out_data.as_mut_ptr(),
                };
                freerdp_sys::smartcard_pack_control_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_get_attrib(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    let call = unsafe { &operation.call.getAttrib };
    let card_handle = get_card_handle(&call.handles.hCard);
    let attr_id = call.dwAttrId;
    log::debug!(
        "smartcard: GET_ATTRIB card=0x{:X} attr_id=0x{:X}",
        card_handle.raw(),
        attr_id
    );

    match integration.get_attrib(&card_handle, attr_id) {
        Ok(mut attr_data) => {
            log::debug!(
                "smartcard: get_attrib success, response_len={}",
                attr_data.len()
            );
            unsafe {
                let ret = freerdp_sys::GetAttrib_Return {
                    ReturnCode: SCARD_S_SUCCESS as i32,
                    cbAttrLen: attr_data.len() as u32,
                    pbAttr: attr_data.as_mut_ptr(),
                };
                freerdp_sys::smartcard_pack_get_attrib_return(out, &ret, attr_id, call.cbAttrLen);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_set_attrib(
    integration: &Arc<dyn SmartcardIntegration>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
) -> u32 {
    let call = unsafe { &operation.call.setAttrib };
    let card_handle = get_card_handle(&call.handles.hCard);
    let attr_id = call.dwAttrId;
    log::debug!(
        "smartcard: SET_ATTRIB card=0x{:X} attr_id=0x{:X} len={}",
        card_handle.raw(),
        attr_id,
        call.cbAttrLen
    );

    let data: &[u8] = if call.pbAttr.is_null() || call.cbAttrLen == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(call.pbAttr, call.cbAttrLen as usize) }
    };

    match integration.set_attrib(&card_handle, attr_id, data) {
        Ok(()) => SCARD_S_SUCCESS,
        Err(code) => code,
    }
}

fn handle_get_status_change(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    let ctx = {
        let ctx_map = contexts.lock().unwrap();
        match ctx_map.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    let (timeout_ms, reader_states_in) = unsafe {
        if unicode {
            let call = &operation.call.getStatusChangeW;
            (
                call.dwTimeOut,
                get_reader_states_in_w(call.rgReaderStates, call.cReaders),
            )
        } else {
            let call = &operation.call.getStatusChangeA;
            (
                call.dwTimeOut,
                get_reader_states_in_a(call.rgReaderStates, call.cReaders),
            )
        }
    };

    let timeout = if timeout_ms == 0xFFFF_FFFF {
        Duration::from_secs(3600 * 24)
    } else {
        Duration::from_millis(timeout_ms as u64)
    };

    log::trace!(
        "smartcard: GET_STATUS_CHANGE context=0x{:X} timeout_ms={} count={}",
        ctx_id,
        timeout_ms,
        reader_states_in.len()
    );

    match integration.get_status_change(&ctx, timeout, &reader_states_in) {
        Ok(result_states) => {
            log::trace!(
                "smartcard: get_status_change success, count={}",
                result_states.len()
            );
            let mut returned_states = pack_reader_states_out(&result_states);
            let ret = freerdp_sys::LocateCards_Return {
                ReturnCode: SCARD_S_SUCCESS as i32,
                cReaders: returned_states.len() as u32,
                rgReaderStates: returned_states.as_mut_ptr(),
            };
            unsafe {
                freerdp_sys::smartcard_pack_get_status_change_return(
                    out,
                    &ret,
                    if unicode { 1 } else { 0 },
                );
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

fn handle_locate_cards(
    _operation: &freerdp_sys::SMARTCARD_OPERATION,
    _out: *mut freerdp_sys::wStream,
    _unicode: bool,
) -> u32 {
    log::debug!("smartcard: LOCATE_CARDS not supported");
    SCARD_E_UNSUPPORTED_FEATURE
}

fn handle_locate_cards_by_atr(
    integration: &Arc<dyn SmartcardIntegration>,
    contexts: &Arc<Mutex<HashMap<u64, ContextEntry>>>,
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    unicode: bool,
) -> u32 {
    let ctx_id = operation.hContext as u64;
    let ctx = {
        let ctx_map = contexts.lock().unwrap();
        match ctx_map.get(&ctx_id) {
            Some(entry) => entry.context,
            None => return SCARD_E_INVALID_HANDLE,
        }
    };

    let (atrs, reader_states_in) = unsafe {
        if unicode {
            let call = &operation.call.locateCardsByATRW;
            (
                get_atr_masks(call.rgAtrMasks, call.cAtrs),
                get_reader_states_in_w(call.rgReaderStates, call.cReaders),
            )
        } else {
            let call = &operation.call.locateCardsByATRA;
            (
                get_atr_masks(call.rgAtrMasks, call.cAtrs),
                get_reader_states_in_a(call.rgReaderStates, call.cReaders),
            )
        }
    };

    log::debug!(
        "smartcard: LOCATE_CARDS_BY_ATR context=0x{:X} atrs_count={} count={}",
        ctx_id,
        atrs.len(),
        reader_states_in.len()
    );

    match integration.locate_cards_by_atr(&ctx, &atrs, &reader_states_in) {
        Ok(results) => {
            log::debug!(
                "smartcard: locate_cards_by_atr success, count={}",
                results.len()
            );
            let mut returned_states = results
                .iter()
                .map(|r| {
                    let rs_in = reader_states_in
                        .iter()
                        .find(|rs| rs.reader_name == r.reader_name);

                    let mut dw_event_state = r.event_state;
                    if r.atr_match {
                        dw_event_state |= SCARD_STATE_ATRMATCH;
                    }

                    freerdp_sys::ReaderState_Return {
                        dwCurrentState: rs_in.map(|rs| rs.current_state).unwrap_or(0),
                        dwEventState: dw_event_state,
                        cbAtr: 0,
                        rgbAtr: [0u8; 36],
                    }
                })
                .collect::<Vec<_>>();

            let ret = freerdp_sys::LocateCards_Return {
                ReturnCode: SCARD_S_SUCCESS as i32,
                cReaders: returned_states.len() as u32,
                rgReaderStates: returned_states.as_mut_ptr(),
            };

            unsafe {
                freerdp_sys::smartcard_pack_locate_cards_return(out, &ret);
            }
            SCARD_S_SUCCESS
        }
        Err(code) => code,
    }
}

// ---------------------------------------------------------------------------
// GET_TRANSMIT_COUNT
// ---------------------------------------------------------------------------

fn handle_get_transmit_count(
    _operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    log::debug!("smartcard: GET_TRANSMIT_COUNT — returning 0");
    unsafe {
        let ret = freerdp_sys::GetTransmitCount_Return {
            ReturnCode: SCARD_S_SUCCESS as i32,
            cTransmitCount: 0,
        };
        freerdp_sys::smartcard_pack_get_transmit_count_return(out, &ret);
    }
    SCARD_S_SUCCESS
}

// ---------------------------------------------------------------------------
// GET_DEVICE_TYPE_ID
// ---------------------------------------------------------------------------

fn handle_get_device_type_id(
    _operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
) -> u32 {
    // RDPDR_DTYP_SMARTCARD = 0x0020
    log::debug!("smartcard: GET_DEVICE_TYPE_ID — returning 0x0020");
    unsafe {
        use crate::addins::smartcard::device::stream_write_u32;
        stream_write_u32(out, SCARD_S_SUCCESS);
        stream_write_u32(out, 0x0020);
    }
    SCARD_S_SUCCESS
}

// ===========================================================================
// READ_CACHE / WRITE_CACHE — key-value store so msclmd doesn't loop
// ===========================================================================

static CARD_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<Vec<u16>, Vec<u8>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

unsafe fn widestr_to_vec(ptr: *const u16) -> Vec<u16> {
    if ptr.is_null() {
        return Vec::new();
    }
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

fn handle_write_cache(operation: &freerdp_sys::SMARTCARD_OPERATION, _is_wide: bool) -> u32 {
    let call = unsafe { &operation.call.writeCacheW };
    let name = unsafe { widestr_to_vec(call.szLookupName) };
    let pb = call.Common.pbData;
    let cb = call.Common.cbDataLen;
    if pb.is_null() || cb == 0 {
        log::debug!("smartcard: WRITE_CACHE — empty");
        return SCARD_S_SUCCESS;
    }
    let data = unsafe { std::slice::from_raw_parts(pb, cb as usize) };
    log::debug!(
        "smartcard: WRITE_CACHE — storing {} bytes (key len={})",
        data.len(),
        name.len()
    );
    CARD_CACHE.lock().unwrap().insert(name, data.to_vec());
    SCARD_S_SUCCESS
}

fn handle_read_cache(
    operation: &freerdp_sys::SMARTCARD_OPERATION,
    out: *mut freerdp_sys::wStream,
    _is_wide: bool,
) -> u32 {
    let call = unsafe { &operation.call.readCacheW };
    let name = unsafe { widestr_to_vec(call.szLookupName) };
    log::debug!("smartcard: READ_CACHE — NOT_FOUND (key len={})", name.len());
    unsafe {
        let ret = freerdp_sys::ReadCache_Return {
            ReturnCode: 0x80100070u32 as i32,
            cbDataLen: 0,
            pbData: std::ptr::null_mut(),
        };
        freerdp_sys::smartcard_pack_read_cache_return(out, &ret);
    }
    0x80100070u32
}

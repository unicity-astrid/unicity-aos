// SPDX-License-Identifier: GPL-2.0-only
/*
 * Synchronous 9P transport for the AOS RV64 Realm machine.
 *
 * The private SBI call is a scheduling boundary, not a hardware device. The
 * outer Realm copies one complete request out of admitted RAM, serves it
 * through a principal-scoped filesystem capability, writes one complete
 * response back, and only then resumes this hart.
 */

#define pr_fmt(fmt) KBUILD_MODNAME ": " fmt

#include <linux/errno.h>
#include <linux/kernel.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/slab.h>
#include <linux/unaligned.h>
#include <asm/io.h>
#include <asm/sbi.h>
#include <net/9p/9p.h>
#include <net/9p/client.h>
#include <net/9p/transport.h>

#define SBI_EXT_AOS_9P 0x08414f53
#define SBI_FID_AOS_9P_EXCHANGE 0
#define AOS_9P_MAX_MESSAGE (64 * 1024)
#define AOS_9P_WORKSPACE_CHANNEL 2

struct aos_9p_channel {
	u32 id;
};

static int aos_9p_request(struct p9_client *client, struct p9_req_t *req)
{
	struct aos_9p_channel *channel = client->trans;
	struct sbiret result;
	phys_addr_t request_address;
	phys_addr_t response_address;
	u32 response_size;

	if (!channel || req->tc.size < P9_HDRSZ ||
	    req->tc.size > AOS_9P_MAX_MESSAGE ||
	    req->rc.capacity < P9_HDRSZ ||
	    req->rc.capacity > AOS_9P_MAX_MESSAGE)
		return -EINVAL;

	request_address = virt_to_phys(req->tc.sdata);
	response_address = virt_to_phys(req->rc.sdata);
	WRITE_ONCE(req->status, REQ_STATUS_SENT);
	mb();
	result = sbi_ecall(SBI_EXT_AOS_9P, SBI_FID_AOS_9P_EXCHANGE,
			   request_address, req->tc.size,
			   response_address, req->rc.capacity,
			   channel->id, 0);
	mb();
	if (result.error)
		return sbi_err_map_linux_errno(result.error);
	if (result.value < P9_HDRSZ || result.value > req->rc.capacity)
		return -EIO;

	response_size = get_unaligned_le32(req->rc.sdata);
	if (response_size != result.value)
		return -EIO;
	req->rc.size = response_size;
	p9_client_cb(client, req, REQ_STATUS_RCVD);
	return 0;
}

static int aos_9p_cancel(struct p9_client *client, struct p9_req_t *req)
{
	return 1;
}

static int aos_9p_cancelled(struct p9_client *client, struct p9_req_t *req)
{
	p9_req_put(client, req);
	return 0;
}

static int aos_9p_create(struct p9_client *client, const char *devname,
			 char *args)
{
	struct aos_9p_channel *channel;

	if (!devname || strcmp(devname, "workspace"))
		return -ENOENT;
	channel = kzalloc(sizeof(*channel), GFP_KERNEL);
	if (!channel)
		return -ENOMEM;
	channel->id = AOS_9P_WORKSPACE_CHANNEL;
	client->trans = channel;
	client->status = Connected;
	return 0;
}

static void aos_9p_close(struct p9_client *client)
{
	if (!client)
		return;
	client->status = Disconnected;
	kfree(client->trans);
	client->trans = NULL;
}

static struct p9_trans_module aos_9p_transport = {
	.name = "aos",
	.maxsize = AOS_9P_MAX_MESSAGE,
	.pooled_rbuffers = false,
	.def = 0,
	.owner = THIS_MODULE,
	.create = aos_9p_create,
	.close = aos_9p_close,
	.request = aos_9p_request,
	.cancel = aos_9p_cancel,
	.cancelled = aos_9p_cancelled,
};

static int __init aos_9p_init(void)
{
	v9fs_register_trans(&aos_9p_transport);
	return 0;
}

static void __exit aos_9p_exit(void)
{
	v9fs_unregister_trans(&aos_9p_transport);
}

module_init(aos_9p_init);
module_exit(aos_9p_exit);
MODULE_ALIAS_9P("aos");
MODULE_AUTHOR("Unicity Labs");
MODULE_DESCRIPTION("AOS Realm SBI 9P transport");
MODULE_LICENSE("GPL");

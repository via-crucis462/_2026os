//! Linux `rb_root_cached` 风格的 arena 红黑树。

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RbColor {
	/// 新插入节点的初始颜色。
	Red,
	/// 根节点及空叶子采用的颜色。
	Black,
}

/// arena 中的红黑树节点，索引承担 Linux 指针链接的角色。
struct RbNode<K, V> {
	/// 决定节点在树中顺序的键。
	key: K,
	/// 与键关联的调度实体或任务。
	value: V,
	/// 父节点在 arena 中的索引。
	parent: Option<usize>,
	/// 左子节点在 arena 中的索引。
	left: Option<usize>,
	/// 右子节点在 arena 中的索引。
	right: Option<usize>,
	/// 红黑树平衡算法使用的节点颜色。
	color: RbColor,
}

/// 对应 Linux `struct rb_root_cached`，缓存最左节点。
pub(crate) struct RbRootCached<K, V> {
	/// 根节点在 arena 中的索引。
	root: Option<usize>,
	/// 最左节点缓存，对应 Linux `rb_leftmost`。
	leftmost: Option<usize>,
	/// 节点 arena；空槽用于表示已经移除的节点。
	nodes: Vec<Option<RbNode<K, V>>>,
	/// 当前树中有效节点数量。
	len: usize,
}

impl<K: Ord + Copy, V> RbRootCached<K, V> {
	/// 创建根节点和最左缓存均为空的红黑树。
	pub(crate) fn new() -> Self {
		Self {
			root: None,
			leftmost: None,
			nodes: Vec::new(),
			len: 0,
		}
	}

	/// 判断红黑树中是否没有有效节点。
	pub(crate) fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// 以 O(1) 时间返回最左节点保存的值。
	pub(crate) fn first(&self) -> Option<&V> {
		self.leftmost
			.and_then(|index| self.nodes[index].as_ref())
			.map(|node| &node.value)
	}

	/// 以 O(1) 时间返回最左节点的排序键。
	pub(crate) fn first_key(&self) -> Option<K> {
		self.leftmost
			.and_then(|index| self.nodes[index].as_ref())
			.map(|node| node.key)
	}

	/// 返回节点颜色；空节点按红黑树规则视为黑色。
	fn color(&self, index: Option<usize>) -> RbColor {
		index
			.and_then(|index| self.nodes[index].as_ref())
			.map_or(RbColor::Black, |node| node.color)
	}

	/// 返回指定节点的父节点索引。
	fn parent(&self, index: usize) -> Option<usize> {
		self.nodes[index].as_ref().unwrap().parent
	}

	/// 返回指定节点的左子节点索引。
	fn left(&self, index: usize) -> Option<usize> {
		self.nodes[index].as_ref().unwrap().left
	}

	/// 返回指定节点的右子节点索引。
	fn right(&self, index: usize) -> Option<usize> {
		self.nodes[index].as_ref().unwrap().right
	}

	/// 更新节点的父链接；传入空节点时不执行操作。
	fn set_parent(&mut self, index: Option<usize>, parent: Option<usize>) {
		if let Some(index) = index {
			self.nodes[index].as_mut().unwrap().parent = parent;
		}
	}

	/// 以指定节点为轴执行标准红黑树左旋。
	fn rotate_left(&mut self, node: usize) {
		let pivot = self.right(node).expect("left rotation requires a right child");
		let pivot_left = self.left(pivot);
		self.nodes[node].as_mut().unwrap().right = pivot_left;
		self.set_parent(pivot_left, Some(node));
		let parent = self.parent(node);
		self.set_parent(Some(pivot), parent);
		if let Some(parent) = parent {
			if self.left(parent) == Some(node) {
				self.nodes[parent].as_mut().unwrap().left = Some(pivot);
			} else {
				self.nodes[parent].as_mut().unwrap().right = Some(pivot);
			}
		} else {
			self.root = Some(pivot);
		}
		self.nodes[pivot].as_mut().unwrap().left = Some(node);
		self.set_parent(Some(node), Some(pivot));
	}

	/// 以指定节点为轴执行标准红黑树右旋。
	fn rotate_right(&mut self, node: usize) {
		let pivot = self.left(node).expect("right rotation requires a left child");
		let pivot_right = self.right(pivot);
		self.nodes[node].as_mut().unwrap().left = pivot_right;
		self.set_parent(pivot_right, Some(node));
		let parent = self.parent(node);
		self.set_parent(Some(pivot), parent);
		if let Some(parent) = parent {
			if self.left(parent) == Some(node) {
				self.nodes[parent].as_mut().unwrap().left = Some(pivot);
			} else {
				self.nodes[parent].as_mut().unwrap().right = Some(pivot);
			}
		} else {
			self.root = Some(pivot);
		}
		self.nodes[pivot].as_mut().unwrap().right = Some(node);
		self.set_parent(Some(node), Some(pivot));
	}

	/// 按键插入节点，更新最左缓存并恢复红黑树性质。
	pub(crate) fn insert(&mut self, key: K, value: V) {
		let mut parent = None;
		let mut cursor = self.root;
		// 寻找插入位置，沿途记录父节点索引。
		while let Some(index) = cursor {
			parent = Some(index);
			cursor = if key < self.nodes[index].as_ref().unwrap().key {
				self.left(index)
			} else {
				self.right(index)
			};
		}
		let index = self.nodes.len();
		self.nodes.push(Some(RbNode {
			key,
			value,
			parent,
			left: None,
			right: None,
			color: RbColor::Red,
		}));
		if let Some(parent) = parent {
			if key < self.nodes[parent].as_ref().unwrap().key {
				self.nodes[parent].as_mut().unwrap().left = Some(index);
			} else {
				self.nodes[parent].as_mut().unwrap().right = Some(index);
			}
		} else {
			self.root = Some(index);
		}
		if self.leftmost.map_or(true, |leftmost| key < self.nodes[leftmost].as_ref().unwrap().key) {
			self.leftmost = Some(index);
		}
		self.len += 1;
		self.insert_fixup(index);
	}

	/// 对新插入的红色节点执行重新着色和旋转修复。
	fn insert_fixup(&mut self, mut node: usize) {
		while self.color(self.parent(node)) == RbColor::Red {
			let parent = self.parent(node).unwrap();
			let grandparent = self.parent(parent).unwrap();
			if self.left(grandparent) == Some(parent) {
				let uncle = self.right(grandparent);
				if self.color(uncle) == RbColor::Red {
					self.nodes[parent].as_mut().unwrap().color = RbColor::Black;
					self.nodes[uncle.unwrap()].as_mut().unwrap().color = RbColor::Black;
					self.nodes[grandparent].as_mut().unwrap().color = RbColor::Red;
					node = grandparent;
				} else {
					if self.right(parent) == Some(node) {
						node = parent;
						self.rotate_left(node);
					}
					let parent = self.parent(node).unwrap();
					let grandparent = self.parent(parent).unwrap();
					self.nodes[parent].as_mut().unwrap().color = RbColor::Black;
					self.nodes[grandparent].as_mut().unwrap().color = RbColor::Red;
					self.rotate_right(grandparent);
				}
			} else {
				let uncle = self.left(grandparent);
				if self.color(uncle) == RbColor::Red {
					self.nodes[parent].as_mut().unwrap().color = RbColor::Black;
					self.nodes[uncle.unwrap()].as_mut().unwrap().color = RbColor::Black;
					self.nodes[grandparent].as_mut().unwrap().color = RbColor::Red;
					node = grandparent;
				} else {
					if self.left(parent) == Some(node) {
						node = parent;
						self.rotate_right(node);
					}
					let parent = self.parent(node).unwrap();
					let grandparent = self.parent(parent).unwrap();
					self.nodes[parent].as_mut().unwrap().color = RbColor::Black;
					self.nodes[grandparent].as_mut().unwrap().color = RbColor::Red;
					self.rotate_left(grandparent);
				}
			}
		}
		if let Some(root) = self.root {
			self.nodes[root].as_mut().unwrap().color = RbColor::Black;
		}
	}

	/// 暂用重建完成删除；后续调度热路径可替换为原地 erase/fixup。
	pub(crate) fn remove(&mut self, key: K) -> Option<V> {
		let mut entries = Vec::with_capacity(self.len.saturating_sub(1));
		let mut removed = None;
		for slot in self.nodes.drain(..) {
			if let Some(node) = slot {
				if removed.is_none() && node.key == key {
					removed = Some(node.value);
				} else {
					entries.push((node.key, node.value));
				}
			}
		}
		self.root = None;
		self.leftmost = None;
		self.len = 0;
		for (key, value) in entries {
			self.insert(key, value);
		}
		removed
	}

	/// 移除并返回当前最左节点，即排序键最小的节点。
	pub(crate) fn pop_first(&mut self) -> Option<V> {
		let key = self.first_key()?;
		self.remove(key)
	}

	/// 移除第一个满足条件的节点。
	pub(crate) fn remove_where<F>(&mut self, mut predicate: F) -> Option<V>
	where
		F: FnMut(&V) -> bool,
	{
		let key = self.nodes.iter().filter_map(|slot| slot.as_ref())
			.find(|node| predicate(&node.value))
			.map(|node| node.key)?;
		self.remove(key)
	}
}

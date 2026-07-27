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
	/// 已删除节点留下的空槽索引，后续插入优先复用，避免 arena 无限增长。
	free_slots: Vec<usize>,
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
			free_slots: Vec::new(),
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
			.and_then(|index| self.nodes.get(index))
			.and_then(|slot| slot.as_ref())
			.map(|node| &node.value)
	}

	/// 以 O(1) 时间返回最左节点的排序键。
	pub(crate) fn first_key(&self) -> Option<K> {
		self.leftmost
			.and_then(|index| self.nodes.get(index))
			.and_then(|slot| slot.as_ref())
			.map(|node| node.key)
	}

	/// 返回节点颜色；空节点按红黑树规则视为黑色。
	fn color(&self, index: Option<usize>) -> RbColor {
		index
			.and_then(|index| self.nodes.get(index))
			.and_then(|slot| slot.as_ref())
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

	/// 更新节点颜色；空叶子不需要保存颜色。
	fn set_color(&mut self, index: Option<usize>, color: RbColor) {
		if let Some(index) = index {
			self.nodes[index].as_mut().unwrap().color = color;
		}
	}

	/// 返回可选节点的左孩子，空叶子的孩子仍为空。
	fn left_of(&self, index: Option<usize>) -> Option<usize> {
		index.and_then(|index| self.left(index))
	}

	/// 返回可选节点的右孩子，空叶子的孩子仍为空。
	fn right_of(&self, index: Option<usize>) -> Option<usize> {
		index.and_then(|index| self.right(index))
	}

	/// 返回以指定节点为根的子树中键最小的节点。
	fn minimum(&self, mut node: usize) -> usize {
		while let Some(left) = self.left(node) {
			node = left;
		}
		node
	}

	/// 用 replacement 子树替换 node 子树，并维护根和父链接。
	fn transplant(&mut self, node: usize, replacement: Option<usize>) {
		let parent = self.parent(node);
		if let Some(parent) = parent {
			if self.left(parent) == Some(node) {
				self.nodes[parent].as_mut().unwrap().left = replacement;
			} else {
				self.nodes[parent].as_mut().unwrap().right = replacement;
			}
		} else {
			self.root = replacement;
		}
		self.set_parent(replacement, parent);
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
		let node = RbNode {
			key,
			value,
			parent,
			left: None,
			right: None,
			color: RbColor::Red,
		};
		let index = if let Some(index) = self.free_slots.pop() {
			debug_assert!(self.nodes[index].is_none());
			self.nodes[index] = Some(node);
			index
		} else {
			let index = self.nodes.len();
			self.nodes.push(Some(node));
			index
		};
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

	/// 删除黑色节点后修复双黑路径，恢复红黑树的黑高和红色相邻约束。
	///
	/// `node` 可以是空叶子，因此额外传入其父节点；这对应指针实现中
	/// 带 parent 信息的 NIL 节点。
	fn delete_fixup(&mut self, mut node: Option<usize>, mut parent: Option<usize>) {
		while node != self.root && self.color(node) == RbColor::Black {
			let Some(parent_index) = parent else { break };
			if node == self.left(parent_index) {
				let mut sibling = self.right(parent_index);
				if self.color(sibling) == RbColor::Red {
					self.set_color(sibling, RbColor::Black);
					self.set_color(Some(parent_index), RbColor::Red);
					self.rotate_left(parent_index);
					sibling = self.right(parent_index);
				}

				if self.color(self.left_of(sibling)) == RbColor::Black
					&& self.color(self.right_of(sibling)) == RbColor::Black
				{
					self.set_color(sibling, RbColor::Red);
					node = Some(parent_index);
					parent = self.parent(parent_index);
				} else {
					if self.color(self.right_of(sibling)) == RbColor::Black {
						self.set_color(self.left_of(sibling), RbColor::Black);
						self.set_color(sibling, RbColor::Red);
						if let Some(sibling_index) = sibling {
							self.rotate_right(sibling_index);
						}
						sibling = self.right(parent_index);
					}
					self.set_color(sibling, self.color(Some(parent_index)));
					self.set_color(Some(parent_index), RbColor::Black);
					self.set_color(self.right_of(sibling), RbColor::Black);
					self.rotate_left(parent_index);
					node = self.root;
					parent = None;
				}
			} else {
				let mut sibling = self.left(parent_index);
				if self.color(sibling) == RbColor::Red {
					self.set_color(sibling, RbColor::Black);
					self.set_color(Some(parent_index), RbColor::Red);
					self.rotate_right(parent_index);
					sibling = self.left(parent_index);
				}

				if self.color(self.right_of(sibling)) == RbColor::Black
					&& self.color(self.left_of(sibling)) == RbColor::Black
				{
					self.set_color(sibling, RbColor::Red);
					node = Some(parent_index);
					parent = self.parent(parent_index);
				} else {
					if self.color(self.left_of(sibling)) == RbColor::Black {
						self.set_color(self.right_of(sibling), RbColor::Black);
						self.set_color(sibling, RbColor::Red);
						if let Some(sibling_index) = sibling {
							self.rotate_left(sibling_index);
						}
						sibling = self.left(parent_index);
					}
					self.set_color(sibling, self.color(Some(parent_index)));
					self.set_color(Some(parent_index), RbColor::Black);
					self.set_color(self.left_of(sibling), RbColor::Black);
					self.rotate_right(parent_index);
					node = self.root;
					parent = None;
				}
			}
		}
		self.set_color(node, RbColor::Black);
	}

	/// 按键查找并原地删除节点，时间复杂度为 O(log n)。
	pub(crate) fn remove(&mut self, key: K) -> Option<V> {
		let mut cursor = self.root;
		let target = loop {
			let index = cursor?;
			let node_key = self.nodes[index].as_ref().unwrap().key;
			if key == node_key {
				break index;
			}
			cursor = if key < node_key { self.left(index) } else { self.right(index) };
		};

		let mut moved = target;
		let mut removed_color = self.color(Some(moved));
		let replacement;
		let replacement_parent;

		if self.left(target).is_none() {
			replacement = self.right(target);
			replacement_parent = self.parent(target);
			self.transplant(target, replacement);
		} else if self.right(target).is_none() {
			replacement = self.left(target);
			replacement_parent = self.parent(target);
			self.transplant(target, replacement);
		} else {
			moved = self.minimum(self.right(target).unwrap());
			removed_color = self.color(Some(moved));
			replacement = self.right(moved);
			if self.parent(moved) == Some(target) {
				replacement_parent = Some(moved);
				self.set_parent(replacement, Some(moved));
			} else {
				replacement_parent = self.parent(moved);
				self.transplant(moved, replacement);
				let target_right = self.right(target);
				self.nodes[moved].as_mut().unwrap().right = target_right;
				self.set_parent(target_right, Some(moved));
			}
			self.transplant(target, Some(moved));
			let target_left = self.left(target);
			self.nodes[moved].as_mut().unwrap().left = target_left;
			self.set_parent(target_left, Some(moved));
			self.nodes[moved].as_mut().unwrap().color = self.color(Some(target));
		}

		let removed = self.nodes[target].take().unwrap().value;
		self.free_slots.push(target);
		self.len = self.len.saturating_sub(1);
		self.leftmost = self.root.map(|root| self.minimum(root));
		if removed_color == RbColor::Black {
			self.delete_fixup(replacement, replacement_parent);
		}
		Some(removed)
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

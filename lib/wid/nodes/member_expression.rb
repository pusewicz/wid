module Wid
  module Nodes
    class MemberExpression < Expression
      attr_reader :computed, :object, :property

      def initialize(computed, object, property)
        @computed = computed
        @object = object
        @property = property
      end

      def ==(other) = self.class == other.class && computed == other.computed && object == other.object && property == other.property

      def children
        [@object, @property]
      end

      def to_hash
        {class: self.class, computed: @computed, object: @object.to_hash, property: @property.to_hash}
      end
    end
  end
end

module Wid
  module Nodes
    class FunctionDeclaration < Node
      attr_reader :name, :params, :body

      def initialize(name, params, body)
        @name = name
        @params = params
        @body = body
      end

      def children
        [@name, @params, @body]
      end

      def to_hash
        {class: self.class, name: @name.to_hash, params: @params.map(&:to_hash), body: @body.to_hash}
      end
    end
  end
end
